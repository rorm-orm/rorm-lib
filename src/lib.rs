//! This crate is used to provide C bindings for the `rorm-db` crate.
#![deny(missing_docs)]

/// Utility module to provide errors
pub mod errors;
/// Module that holds the definitions for conditions.
pub mod representations;
/// Utility functions and structs such as the ffi safe string implementation.
pub mod utils;

use std::sync::Mutex;
use std::time::Duration;

use futures::StreamExt;
use rorm_db::database::{
    delete, insert, insert_bulk, insert_bulk_returning, insert_returning, query, update,
    ColumnSelector, JoinTable,
};
use rorm_db::row::Row;
use rorm_db::sql::aggregation::SelectAggregator;
use rorm_db::sql::join_table::JoinType;
use rorm_db::sql::ordering::OrderByEntry;
use rorm_db::sql::value::Value;
use rorm_db::transaction::Transaction;
use rorm_db::{executor, Database, DatabaseConfiguration};
use tokio::runtime::Runtime;

use crate::errors::Error;
use crate::representations::{
    DBConnectOptions, FFIColumnSelector, FFICondition, FFIJoin, FFILimitClause, FFIOrderByEntry,
    FFIUpdate, FFIValue,
};
use crate::utils::{
    get_data_from_row, FFIDate, FFIDateTime, FFIOption, FFISlice, FFIString, FFITime, Stream,
    VoidPtr,
};

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

// ----------------------
// FFI Functions below here.
// ----------------------

// --------
// RUNTIME
// --------
/**
This function is used to initialize and start the async runtime.

It is needed as rorm is completely written asynchronously.

**Important**:
Do not forget to stop the runtime using [rorm_runtime_shutdown]!

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_runtime_start(
    callback: Option<unsafe extern "C" fn(VoidPtr, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be null");

    match RUNTIME.lock() {
        Ok(mut guard) => {
            let rt_opt: &mut Option<Runtime> = &mut guard;
            match Runtime::new() {
                Ok(rt) => {
                    *rt_opt = Some(rt);
                    #[cfg(feature = "logging")]
                    env_logger::init();

                    unsafe { cb(context, Error::NoError) }
                }
                Err(err) => unsafe {
                    cb(
                        context,
                        Error::RuntimeError(err.to_string().as_str().into()),
                    )
                },
            };
        }
        Err(err) => unsafe {
            cb(
                context,
                Error::RuntimeError(err.to_string().as_str().into()),
            )
        },
    }
}

/**
Shutdown the runtime.

Specify the amount of time to wait in milliseconds.

If no runtime is currently existing, a [Error::MissingRuntimeError] will be returned.
If the runtime could not be locked, a [Error::RuntimeError]
containing further information will be returned.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_runtime_shutdown(
    duration: u64,
    callback: Option<unsafe extern "C" fn(VoidPtr, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be null");

    match RUNTIME.lock() {
        Ok(mut guard) => match guard.take() {
            Some(rt) => {
                rt.shutdown_timeout(Duration::from_millis(duration));
                unsafe { cb(context, Error::NoError) }
            }
            None => unsafe { cb(context, Error::MissingRuntimeError) },
        },
        Err(err) => unsafe {
            cb(
                context,
                Error::RuntimeError(err.to_string().as_str().into()),
            )
        },
    };
}

// --------
// DATABASE
// --------

/**
Connect to the database using the provided [DBConnectOptions].

You must provide a callback with the following parameters:

The first parameter is the `context` pointer.
The second parameter will be a pointer to the Database connection.
It will be needed to make queries.
The last parameter holds an [Error] enum.

**Important**:
Rust does not manage the memory of the database.
To properly free it, use [rorm_db_free].

This function is called from an asynchronous context.
*/
#[no_mangle]
pub extern "C" fn rorm_db_connect(
    options: DBConnectOptions,
    callback: Option<unsafe extern "C" fn(VoidPtr, Option<Box<Database>>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be null");

    let db_options_conv: Result<DatabaseConfiguration, Error> = options.into();
    if db_options_conv.is_err() {
        unsafe { cb(context, None, db_options_conv.err().unwrap()) }
        return;
    }
    let db_options = db_options_conv.unwrap();

    let fut = async move {
        match Database::connect(db_options).await {
            Ok(db) => unsafe { cb(context, Some(Box::new(db)), Error::NoError) },
            Err(e) => {
                let error = e.to_string();
                unsafe {
                    cb(
                        context,
                        None,
                        Error::RuntimeError(FFIString::from(error.as_str())),
                    )
                }
            }
        };
    };

    let f = |err: String| {
        unsafe { cb(context, None, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, None, Error::MissingRuntimeError), f);
}

/**
Free the connection to the database.

Takes the pointer to the database instance.

**Important**:
Do not call this function more than once!

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_db_free(_: Box<Database>) {}

// --------
// TRANSACTION
// --------

/**
Starts a transaction on the current database connection.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `callback`: callback function. Takes the `context`, a pointer to a transaction and an [Error].
- `context`: Pass through void pointer.

**Important**:
Rust does not manage the memory of the transaction.
To properly free it, use [rorm_transaction_commit] or [rorm_transaction_abort].

This function is called from an asynchronous context.
*/
#[no_mangle]
pub extern "C" fn rorm_db_start_transaction(
    db: &'static Database,
    callback: Option<unsafe extern "C" fn(VoidPtr, Option<Box<Transaction>>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let fut = async move {
        match db.start_transaction().await {
            Ok(t) => unsafe { cb(context, Some(Box::new(t)), Error::NoError) },
            Err(err) => unsafe {
                let ffi_err = err.to_string();
                cb(context, None, Error::DatabaseError(ffi_err.as_str().into()));
            },
        }
    };

    let f = |err: String| {
        unsafe { cb(context, None, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, None, Error::MissingRuntimeError), f);
}

/**
Commits a transaction.

All previous operations will be applied to the database.

**Parameter**:
- `transaction`: Pointer to a valid transaction, provided by [rorm_db_start_transaction].
- `callback`: callback function. Takes the `context` and an [Error].
- `context`: Pass through void pointer.

**Important**:
Rust takes ownership of `transaction` and frees it after using.
Don't use it anywhere else after calling this function!

This function is called from an asynchronous context.
*/
#[no_mangle]
pub extern "C" fn rorm_transaction_commit(
    transaction: Option<Box<Transaction>>,
    callback: Option<unsafe extern "C" fn(VoidPtr, Error) -> ()>,
    context: VoidPtr,
) {
    let transaction = transaction.expect("Transaction must not be empty");
    let cb = callback.expect("Callback must not be empty");

    let fut = async move {
        match transaction.rollback().await {
            Ok(_) => unsafe { cb(context, Error::NoError) },
            Err(err) => unsafe {
                let ffi_err = err.to_string();
                cb(context, Error::DatabaseError(ffi_err.as_str().into()));
            },
        }
    };

    let f = |err: String| {
        unsafe { cb(context, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, Error::MissingRuntimeError), f);
}

/**
Rollback a transaction and abort it.

All previous operations will be discarded.

**Parameter**:
- `transaction`: Pointer to a valid transaction, provided by [rorm_db_start_transaction].
- `callback`: callback function. Takes the `context` and an [Error].
- `context`: Pass through void pointer.

**Important**:
Rust takes ownership of `transaction` and frees it after using.
Don't use it anywhere else after calling this function!

This function is called from an asynchronous context.
 */
#[no_mangle]
pub extern "C" fn rorm_transaction_rollback(
    transaction: Option<Box<Transaction>>,
    callback: Option<unsafe extern "C" fn(VoidPtr, Error) -> ()>,
    context: VoidPtr,
) {
    let transaction = transaction.expect("Transaction must not be empty");
    let cb = callback.expect("Callback must not be empty");

    let fut = async move {
        match transaction.commit().await {
            Ok(_) => unsafe { cb(context, Error::NoError) },
            Err(err) => unsafe {
                let ffi_err = err.to_string();
                cb(context, Error::DatabaseError(ffi_err.as_str().into()));
            },
        }
    };

    let f = |err: String| {
        unsafe { cb(context, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, Error::MissingRuntimeError), f);
}

// --------
// SQL
// --------

/**
This function executes a raw SQL statement.

Statements are executed as prepared statements, if possible.

To define placeholders, use `?` in SQLite and MySQL and $1, $n in Postgres.
The corresponding parameters are bound in order to the query.

The number of placeholder must match with the number of provided bind parameters.

To include the statement in a transaction specify `transaction` as a valid
Transaction. As the Transaction needs to be mutable, it is important to not
use the Transaction anywhere else until the callback is finished.

If the statement should be executed **not** in a Transaction,
specify a null pointer.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `query_string`: SQL statement to execute.
- `bind_params`: Slice of FFIValues to bind to the query.
- `callback`: callback function. Takes the `context`, a pointer to a slice of rows and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure `db`, `query_string` and `bind_params` are allocated until
the callback was executed.
- The FFISlice returned in the callback is freed after the callback has ended.

This function is called from an asynchronous context.
*/
#[no_mangle]
pub extern "C" fn rorm_db_raw_sql(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    query_string: FFIString<'static>,
    bind_params: FFISlice<'static, FFIValue<'static>>,
    callback: Option<unsafe extern "C" fn(VoidPtr, FFISlice<Row>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let Ok(query_str) = query_string.try_into() else {
        unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
        return;
    };

    let bind_params = {
        let ffi_slice: &[FFIValue] = bind_params.into();
        let mut values: Vec<Value> = vec![];
        for x in ffi_slice {
            let Ok(v) = x.try_into() else {
                unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                return;
            };
            values.push(v);
        }
        values
    };

    let fut = async move {
        let query_res = db
            .raw_sql(query_str, Some(bind_params.as_slice()), transaction)
            .await;

        match query_res {
            Ok(res) => {
                let slice = res.as_slice().into();
                unsafe { cb(context, slice, Error::NoError) };
            }
            Err(err) => unsafe {
                let s = err.to_string();
                cb(
                    context,
                    FFISlice::empty(),
                    Error::DatabaseError(s.as_str().into()),
                )
            },
        };
    };

    let f = |err: String| {
        unsafe {
            cb(
                context,
                FFISlice::empty(),
                Error::RuntimeError(err.as_str().into()),
            )
        };
    };
    spawn_fut!(
        fut,
        cb(context, FFISlice::empty(), Error::MissingRuntimeError),
        f
    );
}

// --------
// QUERY
// --------

/**
This function queries the database given the provided parameter and returns one matched row.

To include the statement in a transaction specify `transaction` as a valid
Transaction. As the Transaction needs to be mutable, it is important to not
use the Transaction anywhere else until the callback is finished.

If the statement should be executed **not** in a Transaction,
specify a null pointer.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `columns`: Array of columns to retrieve from the database.
- `joins`: Array of joins to add to the query.
- `condition`: Pointer to a [Condition].
- `order_by`: Array of [FFIOrderByEntry].
- `offset`: Optional offset to set to the query.
- `callback`: callback function. Takes the `context`, a pointer to a row and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure that `db`, `model`, `columns`, `joins` and `condition` are
allocated until the callback is executed.

This function is called from an asynchronous context.
 */
#[no_mangle]
pub extern "C" fn rorm_db_query_one(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    columns: FFISlice<'static, FFIColumnSelector<'static>>,
    joins: FFISlice<'static, FFIJoin<'static>>,
    condition: Option<&'static FFICondition>,
    order_by: FFISlice<'static, FFIOrderByEntry<'static>>,
    offset: FFIOption<u64>,
    callback: Option<unsafe extern "C" fn(VoidPtr, Option<Box<Row>>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let Ok(model_conv) = model.try_into() else {
        unsafe { cb(context, None, Error::InvalidStringError) };
        return;
    };

    let offset = offset.into();

    let mut column_vec = vec![];
    {
        let column_slice: &[FFIColumnSelector] = columns.into();
        for x in column_slice {
            let table_name_conv: Option<FFIString> = (&x.table_name).into();
            let table_name = match table_name_conv {
                None => None,
                Some(v) => {
                    let Ok(s) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(s)
                }
            };

            let Ok(column_name) = x.column_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let select_alias_conv: Option<FFIString> = (&x.select_alias).into();
            let select_alias = match select_alias_conv {
                None => None,
                Some(v) => {
                    let Ok(s) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(s)
                }
            };

            let aggregation: Option<SelectAggregator> = x.aggregation.into();

            column_vec.push(ColumnSelector {
                table_name,
                column_name,
                select_alias,
                aggregation,
            });
        }
    }

    let mut join_tuple: Vec<(JoinType, &str, &str, rorm_db::sql::conditional::Condition)> = vec![];
    {
        let join_slice: &[FFIJoin] = joins.into();
        for x in join_slice {
            let join_type = x.join_type.into();

            let Ok(table_name) = x.table_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let Ok(join_alias) = x.join_alias.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let join_condition: rorm_db::sql::conditional::Condition =
                match x.join_condition.try_into() {
                    Err(err) => match err {
                        Error::InvalidStringError
                        | Error::InvalidDateError
                        | Error::InvalidTimeError
                        | Error::InvalidDateTimeError => {
                            unsafe { cb(context, None, err) };
                            return;
                        }
                        _ => unreachable!("This error should never occur"),
                    },
                    Ok(v) => v,
                };

            join_tuple.push((join_type, table_name, join_alias, join_condition));
        }
    }

    let cond = if let Some(cond) = condition {
        let cond_conv = cond.try_into();
        if cond_conv.is_err() {
            match cond_conv.as_ref().err().unwrap() {
                Error::InvalidStringError
                | Error::InvalidDateError
                | Error::InvalidTimeError
                | Error::InvalidDateTimeError => unsafe {
                    cb(context, None, cond_conv.err().unwrap())
                },
                _ => {}
            }
            return;
        }
        Some(cond_conv.unwrap())
    } else {
        None
    };

    let mut order_by_vec = vec![];
    {
        let order_by_slice: &[FFIOrderByEntry] = order_by.into();
        for x in order_by_slice {
            let ordering = x.ordering.into();
            let Ok(column_name) = x.column_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let table_name = match x.table_name {
                FFIOption::None => None,
                FFIOption::Some(v) => {
                    let Ok(tn) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(tn)
                }
            };

            order_by_vec.push(OrderByEntry {
                ordering,
                table_name,
                column_name,
            })
        }
    }

    let fut = async move {
        let join_vec: Vec<JoinTable> = join_tuple
            .iter()
            .map(|(a, b, c, d)| JoinTable {
                join_type: *a,
                table_name: b,
                join_alias: c,
                join_condition: d,
            })
            .collect();
        match cond {
            None => {
                if let Some(tx) = transaction {
                    match query::<executor::One>(
                        tx,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        None,
                        &order_by_vec,
                        offset,
                    )
                    .await
                    {
                        Ok(v) => unsafe { cb(context, Some(Box::new(v)), Error::NoError) },
                        Err(err) => {
                            let ffi_str = err.to_string();
                            unsafe {
                                cb(context, None, Error::DatabaseError(ffi_str.as_str().into()))
                            };
                        }
                    };
                } else {
                    match query::<executor::One>(
                        db,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        None,
                        &order_by_vec,
                        offset,
                    )
                    .await
                    {
                        Ok(v) => unsafe { cb(context, Some(Box::new(v)), Error::NoError) },
                        Err(err) => {
                            let ffi_str = err.to_string();
                            unsafe {
                                cb(context, None, Error::DatabaseError(ffi_str.as_str().into()))
                            };
                        }
                    }
                }
            }
            Some(c) => {
                if let Some(tx) = transaction {
                    match query::<executor::One>(
                        tx,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        Some(&c),
                        &order_by_vec,
                        offset,
                    )
                    .await
                    {
                        Ok(v) => unsafe { cb(context, Some(Box::new(v)), Error::NoError) },
                        Err(err) => {
                            let ffi_str = err.to_string();
                            unsafe {
                                cb(context, None, Error::DatabaseError(ffi_str.as_str().into()))
                            }
                        }
                    }
                } else {
                    match query::<executor::One>(
                        db,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        Some(&c),
                        &order_by_vec,
                        offset,
                    )
                    .await
                    {
                        Ok(v) => unsafe { cb(context, Some(Box::new(v)), Error::NoError) },
                        Err(err) => {
                            let ffi_str = err.to_string();
                            unsafe {
                                cb(context, None, Error::DatabaseError(ffi_str.as_str().into()))
                            }
                        }
                    }
                }
            }
        }
    };

    let f = |err: String| {
        unsafe { cb(context, None, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, None, Error::MissingRuntimeError), f);
}

/**
This function queries the database given the provided parameter and returns one optional matched row.
If no results could be retrieved, None is returned.

To include the statement in a transaction specify `transaction` as a valid
Transaction. As the Transaction needs to be mutable, it is important to not
use the Transaction anywhere else until the callback is finished.

If the statement should be executed **not** in a Transaction,
specify a null pointer.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `columns`: Array of columns to retrieve from the database.
- `joins`: Array of joins to add to the query.
- `condition`: Pointer to a [Condition].
- `order_by`: Array of [FFIOrderByEntry].
- `offset`: Optional offset to set to the query.
- `callback`: callback function. Takes the `context`, a pointer to a row and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure that `db`, `model`, `columns`, `joins` and `condition` are
allocated until the callback is executed.

This function is called from an asynchronous context.
 */
#[no_mangle]
pub extern "C" fn rorm_db_query_optional(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    columns: FFISlice<'static, FFIColumnSelector<'static>>,
    joins: FFISlice<'static, FFIJoin<'static>>,
    condition: Option<&'static FFICondition>,
    order_by: FFISlice<'static, FFIOrderByEntry<'static>>,
    offset: FFIOption<u64>,
    callback: Option<unsafe extern "C" fn(VoidPtr, Option<Box<Row>>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let Ok(model_conv) = model.try_into() else {
        unsafe { cb(context, None, Error::InvalidStringError) };
        return;
    };

    let offset = offset.into();

    let mut column_vec = vec![];
    {
        let column_slice: &[FFIColumnSelector] = columns.into();
        for x in column_slice {
            let table_name_conv: Option<FFIString> = (&x.table_name).into();
            let table_name = match table_name_conv {
                None => None,
                Some(v) => {
                    let Ok(s) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(s)
                }
            };

            let Ok(column_name) = x.column_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let select_alias_conv: Option<FFIString> = (&x.select_alias).into();
            let select_alias = match select_alias_conv {
                None => None,
                Some(v) => {
                    let Ok(s) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(s)
                }
            };

            let aggregation: Option<SelectAggregator> = x.aggregation.into();

            column_vec.push(ColumnSelector {
                table_name,
                column_name,
                select_alias,
                aggregation,
            });
        }
    }

    let mut join_tuple: Vec<(JoinType, &str, &str, rorm_db::sql::conditional::Condition)> = vec![];
    {
        let join_slice: &[FFIJoin] = joins.into();
        for x in join_slice {
            let join_type = x.join_type.into();

            let Ok(table_name) = x.table_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let Ok(join_alias) = x.join_alias.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let join_condition: rorm_db::sql::conditional::Condition =
                match x.join_condition.try_into() {
                    Err(err) => match err {
                        Error::InvalidStringError
                        | Error::InvalidDateError
                        | Error::InvalidTimeError
                        | Error::InvalidDateTimeError => {
                            unsafe { cb(context, None, err) };
                            return;
                        }
                        _ => unreachable!("This error should never occur"),
                    },
                    Ok(v) => v,
                };

            join_tuple.push((join_type, table_name, join_alias, join_condition));
        }
    }

    let cond = if let Some(cond) = condition {
        let cond_conv = cond.try_into();
        if cond_conv.is_err() {
            match cond_conv.as_ref().err().unwrap() {
                Error::InvalidStringError
                | Error::InvalidDateError
                | Error::InvalidTimeError
                | Error::InvalidDateTimeError => unsafe {
                    cb(context, None, cond_conv.err().unwrap())
                },
                _ => {}
            }
            return;
        }
        Some(cond_conv.unwrap())
    } else {
        None
    };

    let mut order_by_vec = vec![];
    {
        let order_by_slice: &[FFIOrderByEntry] = order_by.into();
        for x in order_by_slice {
            let ordering = x.ordering.into();
            let Ok(column_name) = x.column_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let table_name = match x.table_name {
                FFIOption::None => None,
                FFIOption::Some(v) => {
                    let Ok(tn) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(tn)
                }
            };

            order_by_vec.push(OrderByEntry {
                ordering,
                table_name,
                column_name,
            })
        }
    }

    let fut = async move {
        let join_vec: Vec<JoinTable> = join_tuple
            .iter()
            .map(|(a, b, c, d)| JoinTable {
                join_type: *a,
                table_name: b,
                join_alias: c,
                join_condition: d,
            })
            .collect();
        match cond {
            None => {
                if let Some(tx) = transaction {
                    match query::<executor::Optional>(
                        tx,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        None,
                        &order_by_vec,
                        offset,
                    )
                    .await
                    {
                        Ok(v) => match v {
                            None => {
                                unsafe { cb(context, None, Error::NoError) };
                            }
                            Some(row) => {
                                unsafe { cb(context, Some(Box::new(row)), Error::NoError) };
                            }
                        },
                        Err(err) => {
                            let ffi_str = err.to_string();
                            unsafe {
                                cb(context, None, Error::DatabaseError(ffi_str.as_str().into()))
                            };
                        }
                    };
                } else {
                    match query::<executor::Optional>(
                        db,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        None,
                        &order_by_vec,
                        offset,
                    )
                    .await
                    {
                        Ok(v) => match v {
                            None => {
                                unsafe { cb(context, None, Error::NoError) };
                            }
                            Some(row) => {
                                unsafe { cb(context, Some(Box::new(row)), Error::NoError) };
                            }
                        },
                        Err(err) => {
                            let ffi_str = err.to_string();
                            unsafe {
                                cb(context, None, Error::DatabaseError(ffi_str.as_str().into()))
                            };
                        }
                    };
                }
            }
            Some(c) => {
                if let Some(tx) = transaction {
                    match query::<executor::Optional>(
                        tx,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        Some(&c),
                        &order_by_vec,
                        offset,
                    )
                    .await
                    {
                        Ok(v) => match v {
                            None => {
                                unsafe { cb(context, None, Error::NoError) };
                            }
                            Some(row) => {
                                unsafe { cb(context, Some(Box::new(row)), Error::NoError) };
                            }
                        },
                        Err(err) => {
                            let ffi_str = err.to_string();
                            unsafe {
                                cb(context, None, Error::DatabaseError(ffi_str.as_str().into()))
                            }
                        }
                    }
                } else {
                    match query::<executor::Optional>(
                        db,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        Some(&c),
                        &order_by_vec,
                        offset,
                    )
                    .await
                    {
                        Ok(v) => match v {
                            None => {
                                unsafe { cb(context, None, Error::NoError) };
                            }
                            Some(row) => {
                                unsafe { cb(context, Some(Box::new(row)), Error::NoError) };
                            }
                        },
                        Err(err) => {
                            let ffi_str = err.to_string();
                            unsafe {
                                cb(context, None, Error::DatabaseError(ffi_str.as_str().into()))
                            }
                        }
                    }
                }
            }
        }
    };

    let f = |err: String| {
        unsafe { cb(context, None, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, None, Error::MissingRuntimeError), f);
}

/**
This function queries the database given the provided parameter and returns all matched rows.

To include the statement in a transaction specify `transaction` as a valid
Transaction. As the Transaction needs to be mutable, it is important to not
use the Transaction anywhere else until the callback is finished.

If the statement should be executed **not** in a Transaction,
specify a null pointer.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect]
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `columns`: Array of columns to retrieve from the database.
- `joins`: Array of joins to add to the query.
- `condition`: Pointer to a [Condition].
- `order_by`: Array of [FFIOrderByEntry].
- `limit`: Optional limit / offset to set to the query.
- `callback`: callback function. Takes the `context`, a FFISlice of rows and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure that `db`, `model`, `columns`, `joins` and `condition` are
allocated until the callback is executed.
- The FFISlice returned in the callback is freed after the callback has ended.

This function is called from an asynchronous context.
 */
#[no_mangle]
pub extern "C" fn rorm_db_query_all(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    columns: FFISlice<'static, FFIColumnSelector<'static>>,
    joins: FFISlice<'static, FFIJoin<'static>>,
    condition: Option<&'static FFICondition>,
    order_by: FFISlice<'static, FFIOrderByEntry<'static>>,
    limit: FFIOption<FFILimitClause>,
    callback: Option<unsafe extern "C" fn(VoidPtr, FFISlice<&Row>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be null");

    let Ok(model_conv) = model.try_into() else {
        unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
        return;
    };

    let limit = limit.into();

    let mut column_vec = vec![];
    {
        let column_slice: &[FFIColumnSelector] = columns.into();
        for x in column_slice {
            let table_name_conv: Option<FFIString> = (&x.table_name).into();
            let table_name = match table_name_conv {
                None => None,
                Some(v) => {
                    let Ok(s) = v.try_into() else {
                        unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                        return;
                    };
                    Some(s)
                }
            };

            let Ok(column_name) = x.column_name.try_into() else {
                unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                return;
            };

            let select_alias_conv: Option<FFIString> = (&x.select_alias).into();
            let select_alias = match select_alias_conv {
                None => None,
                Some(v) => {
                    let Ok(s) = v.try_into() else {
                        unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                        return;
                    };
                    Some(s)
                }
            };

            let aggregation: Option<SelectAggregator> = x.aggregation.into();

            column_vec.push(ColumnSelector {
                table_name,
                column_name,
                select_alias,
                aggregation,
            });
        }
    }

    let mut join_tuple: Vec<(JoinType, &str, &str, rorm_db::sql::conditional::Condition)> = vec![];
    {
        let join_slice: &[FFIJoin] = joins.into();
        for x in join_slice {
            let join_type = x.join_type.into();

            let Ok(table_name) = x.table_name.try_into() else {
                unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                return;
            };

            let Ok(join_alias) = x.join_alias.try_into() else {
                unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                return;
            };

            let join_condition: rorm_db::sql::conditional::Condition =
                match x.join_condition.try_into() {
                    Err(err) => match err {
                        Error::InvalidStringError
                        | Error::InvalidDateError
                        | Error::InvalidTimeError
                        | Error::InvalidDateTimeError => {
                            unsafe { cb(context, FFISlice::empty(), err) };
                            return;
                        }
                        _ => unreachable!("This error should never occur"),
                    },
                    Ok(v) => v,
                };

            join_tuple.push((join_type, table_name, join_alias, join_condition));
        }
    }

    let cond = if let Some(cond) = condition {
        let cond_conv = cond.try_into();
        if cond_conv.is_err() {
            match cond_conv.as_ref().err().unwrap() {
                Error::InvalidStringError
                | Error::InvalidDateError
                | Error::InvalidTimeError
                | Error::InvalidDateTimeError => unsafe {
                    cb(context, FFISlice::empty(), cond_conv.err().unwrap())
                },
                _ => {}
            }
            return;
        }
        Some(cond_conv.unwrap())
    } else {
        None
    };

    let mut order_by_vec = vec![];
    {
        let order_by_slice: &[FFIOrderByEntry] = order_by.into();
        for x in order_by_slice {
            let ordering = x.ordering.into();
            let Ok(column_name) = x.column_name.try_into() else {
                unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                return;
            };

            let table_name = match x.table_name {
                FFIOption::None => None,
                FFIOption::Some(v) => {
                    let Ok(tn) = v.try_into() else {
                        unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                        return;
                    };
                    Some(tn)
                }
            };

            order_by_vec.push(OrderByEntry {
                ordering,
                table_name,
                column_name,
            })
        }
    }

    let fut = async move {
        let join_vec: Vec<JoinTable> = join_tuple
            .iter()
            .map(|(a, b, c, d)| JoinTable {
                join_type: *a,
                table_name: b,
                join_alias: c,
                join_condition: d,
            })
            .collect();
        let query_res = match cond {
            None => {
                if let Some(tx) = transaction {
                    query::<executor::All>(
                        tx,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        None,
                        &order_by_vec,
                        limit,
                    )
                    .await
                } else {
                    query::<executor::All>(
                        db,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        None,
                        &order_by_vec,
                        limit,
                    )
                    .await
                }
            }
            Some(cond) => {
                if let Some(tx) = transaction {
                    query::<executor::All>(
                        tx,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        Some(&cond),
                        &order_by_vec,
                        limit,
                    )
                    .await
                } else {
                    query::<executor::All>(
                        db,
                        model_conv,
                        column_vec.as_slice(),
                        join_vec.as_slice(),
                        Some(&cond),
                        &order_by_vec,
                        limit,
                    )
                    .await
                }
            }
        };
        match query_res {
            Ok(res) => {
                let rows: Vec<&Row> = res.iter().collect();
                let slice = rows.as_slice().into();
                unsafe { cb(context, slice, Error::NoError) };
            }
            Err(err) => {
                let ffi_err = err.to_string();
                unsafe {
                    cb(
                        context,
                        FFISlice::empty(),
                        Error::RuntimeError(ffi_err.as_str().into()),
                    )
                };
            }
        };
    };

    let f = |err: String| {
        unsafe {
            cb(
                context,
                FFISlice::empty(),
                Error::RuntimeError(err.as_str().into()),
            )
        };
    };
    spawn_fut!(
        fut,
        cb(context, FFISlice::empty(), Error::MissingRuntimeError),
        f
    );
}

/**
This function queries the database given the provided parameter.

Returns a pointer to the created stream.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `columns`: Array of columns to retrieve from the database.
- `joins`: Array of joins to add to the query.
- `condition`: Pointer to a [Condition].
- `order_by`: Array of [FFIOrderByEntry].
- `limit`: Optional limit / offset to set to the query.
- `callback`: callback function. Takes the `context`, a stream pointer and an [Error].
- `context`: Pass through void pointer.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_db_query_stream(
    db: &Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString,
    columns: FFISlice<FFIColumnSelector>,
    joins: FFISlice<'static, FFIJoin<'static>>,
    condition: Option<&FFICondition>,
    order_by: FFISlice<'static, FFIOrderByEntry<'static>>,
    limit: FFIOption<FFILimitClause>,
    callback: Option<unsafe extern "C" fn(VoidPtr, Option<Box<Stream>>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be null");

    let Ok(model_conv) = model.try_into() else {
        unsafe { cb(context, None, Error::InvalidStringError) };
        return;
    };

    let limit = limit.into();

    let mut column_vec = vec![];
    {
        let column_slice: &[FFIColumnSelector] = columns.into();
        for x in column_slice {
            let table_name_conv: Option<FFIString> = (&x.table_name).into();
            let table_name = match table_name_conv {
                None => None,
                Some(v) => {
                    let Ok(s) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(s)
                }
            };

            let Ok(column_name) = x.column_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let select_alias_conv: Option<FFIString> = (&x.select_alias).into();
            let select_alias = match select_alias_conv {
                None => None,
                Some(v) => {
                    let Ok(s) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(s)
                }
            };

            let aggregation: Option<SelectAggregator> = x.aggregation.into();

            column_vec.push(ColumnSelector {
                table_name,
                column_name,
                select_alias,
                aggregation,
            });
        }
    }

    let mut join_tuple: Vec<(JoinType, &str, &str, rorm_db::sql::conditional::Condition)> = vec![];
    {
        let join_slice: &[FFIJoin] = joins.into();
        for x in join_slice {
            let join_type = x.join_type.into();

            let Ok(table_name) = x.table_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let Ok(join_alias) = x.join_alias.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let join_condition: rorm_db::sql::conditional::Condition =
                match x.join_condition.try_into() {
                    Err(err) => match err {
                        Error::InvalidStringError
                        | Error::InvalidDateError
                        | Error::InvalidTimeError
                        | Error::InvalidDateTimeError => {
                            unsafe { cb(context, None, err) };
                            return;
                        }
                        _ => unreachable!("This error should never occur"),
                    },
                    Ok(v) => v,
                };

            join_tuple.push((join_type, table_name, join_alias, join_condition));
        }
    }

    let join_vec: Vec<JoinTable> = join_tuple
        .iter()
        .map(|(a, b, c, d)| JoinTable {
            join_type: *a,
            table_name: b,
            join_alias: c,
            join_condition: d,
        })
        .collect();

    let mut order_by_vec = vec![];
    {
        let order_by_slice: &[FFIOrderByEntry] = order_by.into();
        for x in order_by_slice {
            let ordering = x.ordering.into();
            let Ok(column_name) = x.column_name.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };

            let table_name = match x.table_name {
                FFIOption::None => None,
                FFIOption::Some(v) => {
                    let Ok(tn) = v.try_into() else {
                        unsafe { cb(context, None, Error::InvalidStringError) };
                        return;
                    };
                    Some(tn)
                }
            };

            order_by_vec.push(OrderByEntry {
                ordering,
                table_name,
                column_name,
            })
        }
    }

    let query_stream = match condition {
        None => {
            if let Some(tx) = transaction {
                query::<executor::Stream>(
                    tx,
                    model_conv,
                    column_vec.as_slice(),
                    join_vec.as_slice(),
                    None,
                    &order_by_vec,
                    limit,
                )
            } else {
                query::<executor::Stream>(
                    db,
                    model_conv,
                    column_vec.as_slice(),
                    join_vec.as_slice(),
                    None,
                    &order_by_vec,
                    limit,
                )
            }
        }
        Some(c) => {
            let cond_conv: Result<rorm_db::sql::conditional::Condition, Error> = c.try_into();
            if cond_conv.is_err() {
                match cond_conv.as_ref().err().unwrap() {
                    Error::InvalidStringError
                    | Error::InvalidDateError
                    | Error::InvalidTimeError
                    | Error::InvalidDateTimeError => unsafe {
                        cb(context, None, cond_conv.err().unwrap())
                    },
                    _ => {}
                }
                return;
            }
            if let Some(tx) = transaction {
                query::<executor::Stream>(
                    tx,
                    model_conv,
                    column_vec.as_slice(),
                    join_vec.as_slice(),
                    Some(&cond_conv.unwrap()),
                    &order_by_vec,
                    limit,
                )
            } else {
                query::<executor::Stream>(
                    db,
                    model_conv,
                    column_vec.as_slice(),
                    join_vec.as_slice(),
                    Some(&cond_conv.unwrap()),
                    &order_by_vec,
                    limit,
                )
            }
        }
    };
    unsafe {
        cb(
            context,
            Some(Box::new(query_stream.boxed())),
            Error::NoError,
        )
    }
}

/**
Frees the stream given as parameter.

This function panics if the pointer to the stream is invalid.

**Important**:
Do not call this function more than once!

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_stream_free(_: Box<Stream>) {}

/**
Use this function to retrieve a pointer to a row on a stream.

**Parameter**:
- `stream_ptr`: Mutable pointer to the stream that is obtained from [rorm_db_query_stream].
- `callback`: callback function. Takes the `context`, a row pointer and a [Error].
- `context`: Pass through void pointer

**Important**:
- Do not call this function multiple times on the same stream, unless all given callbacks have
returned successfully. Calling the function multiple times on the same stream will result in
undefined behaviour!
- Do not call this function on the same stream if the previous call
returned a [Error::NoRowsLeftInStream].
- Do not use pass the stream to another function unless the callback of the current call is finished

This function is called from an asynchronous context.
 */
#[no_mangle]
pub extern "C" fn rorm_stream_get_row(
    stream_ptr: &'static mut Stream,
    callback: Option<unsafe extern "C" fn(VoidPtr, Option<Box<Row>>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be null");

    let fut = async move {
        let row_opt = stream_ptr.next().await;
        match row_opt {
            None => unsafe { cb(context, None, Error::NoRowsLeftInStream) },
            Some(row_res) => match row_res {
                Err(err) => unsafe {
                    cb(
                        context,
                        None,
                        Error::DatabaseError(err.to_string().as_str().into()),
                    )
                },
                Ok(row) => unsafe { cb(context, Some(Box::new(row)), Error::NoError) },
            },
        }
    };

    let f = |err: String| {
        unsafe { cb(context, None, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, None, Error::MissingRuntimeError), f);
}

// --------
// DELETE
// --------
/**
This function deletes rows from the database based on the given conditions.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `condition`: Pointer to a [Condition].
- `callback`: callback function. Takes the `context`, a pointer to a vec of rows and an [Error].
- `context`: Pass through void pointer.

Returns the rows affected of the delete statement. Note that this also includes
    relations, etc.

**Important**:
- Make sure that `db`, `model` and `condition` are
allocated until the callback is executed.

This function is called from an asynchronous context.
*/
#[no_mangle]
pub extern "C" fn rorm_db_delete(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    condition: Option<&'static FFICondition>,
    callback: Option<unsafe extern "C" fn(VoidPtr, u64, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be null");

    let Ok(model_conv) = model.try_into() else {
        unsafe { cb(context, u64::MAX, Error::InvalidStringError) };
        return;
    };

    let cond = if let Some(cond) = condition {
        let cond_conv = cond.try_into();
        if cond_conv.is_err() {
            match cond_conv.as_ref().err().unwrap() {
                Error::InvalidStringError
                | Error::InvalidDateError
                | Error::InvalidTimeError
                | Error::InvalidDateTimeError => unsafe {
                    cb(context, u64::MAX, cond_conv.err().unwrap())
                },
                _ => {}
            }
            return;
        }
        Some(cond_conv.unwrap())
    } else {
        None
    };

    let fut = async move {
        match cond {
            None => {
                if let Some(tx) = transaction {
                    match delete(tx, model_conv, None).await {
                        Ok(rows_affected) => unsafe { cb(context, rows_affected, Error::NoError) },
                        Err(err) => {
                            let ffi_err = err.to_string();
                            unsafe {
                                cb(
                                    context,
                                    u64::MAX,
                                    Error::DatabaseError(ffi_err.as_str().into()),
                                )
                            };
                        }
                    }
                } else {
                    match delete(db, model_conv, None).await {
                        Ok(rows_affected) => unsafe { cb(context, rows_affected, Error::NoError) },
                        Err(err) => {
                            let ffi_err = err.to_string();
                            unsafe {
                                cb(
                                    context,
                                    u64::MAX,
                                    Error::DatabaseError(ffi_err.as_str().into()),
                                )
                            };
                        }
                    }
                }
            }
            Some(v) => {
                if let Some(tx) = transaction {
                    match delete(tx, model_conv, Some(&v)).await {
                        Ok(rows_affected) => unsafe { cb(context, rows_affected, Error::NoError) },
                        Err(err) => {
                            let ffi_err = err.to_string();
                            unsafe {
                                cb(
                                    context,
                                    u64::MAX,
                                    Error::DatabaseError(ffi_err.as_str().into()),
                                )
                            };
                        }
                    }
                } else {
                    match delete(db, model_conv, Some(&v)).await {
                        Ok(rows_affected) => unsafe { cb(context, rows_affected, Error::NoError) },
                        Err(err) => {
                            let ffi_err = err.to_string();
                            unsafe {
                                cb(
                                    context,
                                    u64::MAX,
                                    Error::DatabaseError(ffi_err.as_str().into()),
                                )
                            };
                        }
                    }
                }
            }
        }
    };

    let f = |err: String| {
        unsafe { cb(context, u64::MAX, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, u64::MAX, Error::MissingRuntimeError), f);
}

/**
Frees the row given as parameter.

The function panics if the provided pointer is invalid.

**Important**:
Do not call this function on the same pointer more than once!

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_free(_: Box<Row>) {}

// --------
// INSERT
// --------

/**
This functions inserts a row into the database while returning the requested columns.

**Parameter**:
- `db`: Reference tot he Database, provided by [rorm_db_connect]
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `columns`: Array of columns to insert to the table.
- `row`: List of values to insert. Must be of the same length as `columns`.
- `returning`: Array of column to return after for the insert operation.
- `callback`: callback function. Takes the `context` a pointer to a row and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure that `db`, `model`, `columns`, `row` and `returning` are allocated until the callback is executed.
- The Row that is returned in the callback is freed after the callback has ended.

This function is called from an asynchronous context.
*/
#[no_mangle]
pub extern "C" fn rorm_db_insert_returning(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    columns: FFISlice<'static, FFIString<'static>>,
    row: FFISlice<'static, FFIValue<'static>>,
    returning: FFISlice<'static, FFIString<'static>>,
    callback: Option<unsafe extern "C" fn(VoidPtr, Option<Box<Row>>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let Ok(model) = model.try_into() else {
        unsafe { cb(context, None, Error::InvalidStringError) };
        return;
    };

    let mut column_vec = vec![];
    {
        let column_slice: &[FFIString] = columns.into();
        for &x in column_slice {
            let Ok(v) = x.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };
            column_vec.push(v);
        }
    }

    let mut value_vec = vec![];
    {
        let value_slice: &[FFIValue] = row.into();
        for x in value_slice {
            let x_conv = x.try_into();
            if x_conv.is_err() {
                match x_conv.as_ref().err().unwrap() {
                    Error::InvalidStringError
                    | Error::InvalidDateError
                    | Error::InvalidTimeError
                    | Error::InvalidDateTimeError => unsafe {
                        cb(context, None, x_conv.err().unwrap())
                    },
                    _ => {}
                }
                return;
            }
            value_vec.push(x_conv.unwrap());
        }
    }

    let mut returning_vec = vec![];
    {
        let returning_slice: &[FFIString] = returning.into();
        for x in returning_slice {
            let Ok(v) = x.try_into() else {
                unsafe { cb(context, None, Error::InvalidStringError) };
                return;
            };
            returning_vec.push(v);
        }
    }

    let fut = async move {
        if let Some(tx) = transaction {
            match insert_returning(
                tx,
                model,
                column_vec.as_slice(),
                value_vec.as_slice(),
                returning_vec.as_slice(),
            )
            .await
            {
                Err(err) => unsafe {
                    cb(
                        context,
                        None,
                        Error::DatabaseError(err.to_string().as_str().into()),
                    )
                },
                Ok(v) => unsafe { cb(context, Some(Box::new(v)), Error::NoError) },
            };
        } else {
            match insert_returning(
                db,
                model,
                column_vec.as_slice(),
                value_vec.as_slice(),
                returning_vec.as_slice(),
            )
            .await
            {
                Err(err) => unsafe {
                    cb(
                        context,
                        None,
                        Error::DatabaseError(err.to_string().as_str().into()),
                    )
                },
                Ok(v) => unsafe { cb(context, Some(Box::new(v)), Error::NoError) },
            };
        }
    };

    let f = |err: String| {
        unsafe { cb(context, None, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, None, Error::MissingRuntimeError), f);
}

/**
This function inserts multiple rows into the database while returning the requested columns from all
inserted values.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `columns`: Array of columns to insert to the table.
- `rows`: List of list of values to insert. The inner lists must be of the same length as `columns`.
- `returning`: Array of column to return after for the insert operation.
- `callback`: callback function. Takes the `context` and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure that `db`, `model`, `columns`, `rows` and `returning` are allocated until the callback is executed.
- The FFISlice returned in the callback is freed after the callback has ended.

This function is called from an asynchronous context.
 */
#[no_mangle]
pub extern "C" fn rorm_db_insert_bulk_returning(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    columns: FFISlice<'static, FFIString<'static>>,
    rows: FFISlice<'static, FFISlice<'static, FFIValue<'static>>>,
    returning: FFISlice<'static, FFIString<'static>>,
    callback: Option<unsafe extern "C" fn(VoidPtr, FFISlice<&Row>, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let Ok(model) = model.try_into() else {
        unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
        return;
    };

    let mut column_vec: Vec<&str> = vec![];
    {
        let column_slice: &[FFIString] = columns.into();
        for &x in column_slice {
            let Ok(v) = x.try_into() else {
                unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError) };
                return;
            };
            column_vec.push(v);
        }
    }

    let mut rows_vec = vec![];
    {
        let row_slices: &[FFISlice<FFIValue>] = rows.into();
        for row in row_slices {
            let mut row_vec: Vec<Value<'_>> = vec![];
            let row_slice: &[FFIValue] = row.into();
            for x in row_slice {
                let val = x.try_into();
                if val.is_err() {
                    match val.as_ref().err().unwrap() {
                        Error::InvalidStringError
                        | Error::InvalidDateError
                        | Error::InvalidTimeError
                        | Error::InvalidDateTimeError => unsafe {
                            cb(context, FFISlice::empty(), val.err().unwrap())
                        },
                        _ => {}
                    }
                    return;
                }
                row_vec.push(val.unwrap());
            }
            rows_vec.push(row_vec);
        }
    }

    let mut returning_vec: Vec<&str> = vec![];
    {
        let returning_slice: &[FFIString] = returning.into();
        for x in returning_slice {
            let Ok(v) = x.try_into() else {
                unsafe { cb(context, FFISlice::empty(), Error::InvalidStringError )}
                return;
            };

            returning_vec.push(v);
        }
    }

    let fut = async move {
        if let Some(tx) = transaction {
            match insert_bulk_returning(
                tx,
                model,
                column_vec.as_slice(),
                rows_vec
                    .iter()
                    .map(|x| x.as_slice())
                    .collect::<Vec<&[Value]>>()
                    .as_slice(),
                returning_vec.as_slice(),
            )
            .await
            {
                Ok(v) => {
                    let rows: Vec<&Row> = v.iter().collect();
                    let slice = rows.as_slice().into();
                    unsafe { cb(context, slice, Error::NoError) }
                }
                Err(err) => {
                    let ffi_str = err.to_string();
                    unsafe {
                        cb(
                            context,
                            FFISlice::empty(),
                            Error::DatabaseError(ffi_str.as_str().into()),
                        )
                    };
                }
            };
        } else {
            match insert_bulk_returning(
                db,
                model,
                column_vec.as_slice(),
                rows_vec
                    .iter()
                    .map(|x| x.as_slice())
                    .collect::<Vec<&[Value]>>()
                    .as_slice(),
                returning_vec.as_slice(),
            )
            .await
            {
                Ok(v) => {
                    let rows: Vec<&Row> = v.iter().collect();
                    let slice = rows.as_slice().into();
                    unsafe { cb(context, slice, Error::NoError) }
                }
                Err(err) => {
                    let ffi_str = err.to_string();
                    unsafe {
                        cb(
                            context,
                            FFISlice::empty(),
                            Error::DatabaseError(ffi_str.as_str().into()),
                        );
                    };
                }
            };
        }
    };

    let f = |err: String| {
        unsafe {
            cb(
                context,
                FFISlice::empty(),
                Error::RuntimeError(err.as_str().into()),
            )
        };
    };
    spawn_fut!(
        fut,
        cb(context, FFISlice::empty(), Error::MissingRuntimeError),
        f
    );
}

/**
This function inserts a row into the database.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `columns`: Array of columns to insert to the table.
- `row`: List of values to insert. Must be of the same length as `columns`.
- `callback`: callback function. Takes the `context` and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure that `db`, `model`, `columns` and `row` are allocated until the callback is executed.

This function is called from an asynchronous context.
*/
#[no_mangle]
pub extern "C" fn rorm_db_insert(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    columns: FFISlice<'static, FFIString<'static>>,
    row: FFISlice<'static, FFIValue<'static>>,
    callback: Option<unsafe extern "C" fn(VoidPtr, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let Ok(model) = model.try_into() else {
        unsafe { cb(context, Error::InvalidStringError) };
        return;
    };

    let mut column_vec = vec![];
    {
        let column_slice: &[FFIString] = columns.into();
        for &x in column_slice {
            let Ok(v) = x.try_into() else {
                unsafe { cb(context, Error::InvalidStringError) };
                return;
            };
            column_vec.push(v);
        }
    }

    let mut value_vec = vec![];
    {
        let value_slice: &[FFIValue] = row.into();
        for x in value_slice {
            let x_conv = x.try_into();
            if x_conv.is_err() {
                match x_conv.as_ref().err().unwrap() {
                    Error::InvalidStringError
                    | Error::InvalidDateError
                    | Error::InvalidTimeError
                    | Error::InvalidDateTimeError => unsafe { cb(context, x_conv.err().unwrap()) },
                    _ => {}
                }
                return;
            }
            value_vec.push(x_conv.unwrap());
        }
    }

    let fut = async move {
        if let Some(tx) = transaction {
            match insert(tx, model, column_vec.as_slice(), value_vec.as_slice()).await {
                Err(err) => unsafe {
                    cb(
                        context,
                        Error::DatabaseError(err.to_string().as_str().into()),
                    )
                },
                Ok(_) => unsafe { cb(context, Error::NoError) },
            };
        } else {
            match insert(db, model, column_vec.as_slice(), value_vec.as_slice()).await {
                Err(err) => unsafe {
                    cb(
                        context,
                        Error::DatabaseError(err.to_string().as_str().into()),
                    )
                },
                Ok(_) => unsafe { cb(context, Error::NoError) },
            };
        }
    };

    let f = |err: String| {
        unsafe { cb(context, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, Error::MissingRuntimeError), f);
}

/**
This function inserts multiple rows into the database.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `columns`: Array of columns to insert to the table.
- `rows`: List of list of values to insert. The inner lists must be of the same length as `columns`.
- `callback`: callback function. Takes the `context` and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure that `db`, `model`, `columns` and `rows` are allocated until the callback is executed.

This function is called from an asynchronous context.
*/
#[no_mangle]
pub extern "C" fn rorm_db_insert_bulk(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    columns: FFISlice<'static, FFIString<'static>>,
    rows: FFISlice<'static, FFISlice<'static, FFIValue<'static>>>,
    callback: Option<unsafe extern "C" fn(VoidPtr, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let Ok(model) = model.try_into() else {
        unsafe { cb(context, Error::InvalidStringError) };
        return;
    };

    let mut column_vec = vec![];
    {
        let column_slice: &[FFIString] = columns.into();
        for &x in column_slice {
            let Ok(v) = x.try_into() else {
                unsafe { cb(context, Error::InvalidStringError) };
                return;
            };
            column_vec.push(v);
        }
    }

    let mut rows_vec = vec![];
    {
        let row_slices: &[FFISlice<FFIValue>] = rows.into();
        for row in row_slices {
            let mut row_vec = vec![];
            let row_slice: &[FFIValue] = row.into();
            for x in row_slice {
                let val = x.try_into();
                if val.is_err() {
                    match val.as_ref().err().unwrap() {
                        Error::InvalidStringError
                        | Error::InvalidDateError
                        | Error::InvalidTimeError
                        | Error::InvalidDateTimeError => unsafe { cb(context, val.err().unwrap()) },
                        _ => {}
                    }
                    return;
                }
                row_vec.push(val.unwrap());
            }
            rows_vec.push(row_vec);
        }
    }

    let fut = async move {
        if let Some(tx) = transaction {
            match insert_bulk(
                tx,
                model,
                column_vec.as_slice(),
                rows_vec
                    .iter()
                    .map(|x| x.as_slice())
                    .collect::<Vec<&[Value]>>()
                    .as_slice(),
            )
            .await
            {
                Ok(_) => unsafe { cb(context, Error::NoError) },
                Err(err) => {
                    let ffi_str = err.to_string();
                    unsafe { cb(context, Error::DatabaseError(ffi_str.as_str().into())) };
                }
            };
        } else {
            match insert_bulk(
                db,
                model,
                column_vec.as_slice(),
                rows_vec
                    .iter()
                    .map(|x| x.as_slice())
                    .collect::<Vec<&[Value]>>()
                    .as_slice(),
            )
            .await
            {
                Ok(_) => unsafe { cb(context, Error::NoError) },
                Err(err) => {
                    let ffi_str = err.to_string();
                    unsafe { cb(context, Error::DatabaseError(ffi_str.as_str().into())) };
                }
            };
        }
    };

    let f = |err: String| {
        unsafe { cb(context, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, Error::MissingRuntimeError), f);
}

// --------
// UPDATE
// --------

/**
This function updates rows in the database.

**Parameter**:
- `db`: Reference to the Database, provided by [rorm_db_connect].
- `transaction`: Mutable pointer to a Transaction. Can be a null pointer to ignore this parameter.
- `model`: Name of the table to query.
- `updates`: List of [FFIUpdate] to apply.
- `condition`: Pointer to a [Condition].
- `callback`: callback function. Takes the `context`, the rows affected and an [Error].
- `context`: Pass through void pointer.

**Important**:
- Make sure that `db`, `model`, `updates` and `condition`
are allocated until the callback is executed.

This function is called from an asynchronous context.
 */
#[no_mangle]
pub extern "C" fn rorm_db_update(
    db: &'static Database,
    transaction: Option<&'static mut Transaction>,
    model: FFIString<'static>,
    updates: FFISlice<'static, FFIUpdate<'static>>,
    condition: Option<&'static FFICondition>,
    callback: Option<unsafe extern "C" fn(VoidPtr, u64, Error) -> ()>,
    context: VoidPtr,
) {
    let cb = callback.expect("Callback must not be empty");

    let Ok(model) = model.try_into() else {
        unsafe { cb(context, u64::MAX, Error::InvalidStringError) };
        return;
    };

    let mut up = vec![];
    let update_slice: &[FFIUpdate] = updates.into();
    for update in update_slice {
        let Ok(column_conv) = update.column.try_into() else {
            unsafe { cb(context, u64::MAX, Error::InvalidStringError) };
            return;
        };

        let Ok(value_conv) = (&update.value).try_into() else {
            unsafe { cb(context, u64::MAX, Error::InvalidStringError) };
            return;
        };

        up.push((column_conv, value_conv));
    }

    let cond = if let Some(cond) = condition {
        let cond_conv = cond.try_into();
        if cond_conv.is_err() {
            match cond_conv.as_ref().err().unwrap() {
                Error::InvalidStringError
                | Error::InvalidDateError
                | Error::InvalidTimeError
                | Error::InvalidDateTimeError => unsafe {
                    cb(context, u64::MAX, cond_conv.err().unwrap())
                },
                _ => {}
            }
            return;
        }
        Some(cond_conv.unwrap())
    } else {
        None
    };

    let fut = async move {
        let query_res = match cond {
            None => {
                if let Some(tx) = transaction {
                    update(tx, model, up.as_slice(), None).await
                } else {
                    update(db, model, up.as_slice(), None).await
                }
            }
            Some(cond) => {
                if let Some(tx) = transaction {
                    update(tx, model, up.as_slice(), Some(&cond)).await
                } else {
                    update(db, model, up.as_slice(), Some(&cond)).await
                }
            }
        };

        match query_res {
            Ok(res) => unsafe {
                cb(context, res, Error::NoError);
            },
            Err(err) => unsafe {
                let ffi_err = err.to_string();
                cb(
                    context,
                    u64::MAX,
                    Error::DatabaseError(ffi_err.as_str().into()),
                );
            },
        }
    };

    let f = |err: String| {
        unsafe { cb(context, u64::MAX, Error::RuntimeError(err.as_str().into())) };
    };
    spawn_fut!(fut, cb(context, u64::MAX, Error::MissingRuntimeError), f);
}

// --------
// ROW VALUE DECODING
// --------
/**
Tries to retrieve a bool from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
*/
#[no_mangle]
pub extern "C" fn rorm_row_get_bool(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> bool {
    get_data_from_row(false, row_ptr, index, error_ptr)
}

/**
Tries to retrieve an i64 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_i64(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> i64 {
    get_data_from_row(i64::MAX, row_ptr, index, error_ptr)
}

/**
Tries to retrieve an i32 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_i32(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> i32 {
    get_data_from_row(i32::MAX, row_ptr, index, error_ptr)
}

/**
Tries to retrieve an i16 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_i16(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> i16 {
    get_data_from_row(i16::MAX, row_ptr, index, error_ptr)
}

/**
Tries to retrieve an FFISlice of a u8 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
*/
#[no_mangle]
pub extern "C" fn rorm_row_get_binary<'a>(
    row_ptr: &'a Row,
    index: FFIString<'a>,
    error_ptr: &mut Error,
) -> FFISlice<'a, u8> {
    get_data_from_row([0u8; 0].as_slice(), row_ptr, index, error_ptr)
}

/**
Tries to retrieve an f32 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_f32(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> f32 {
    get_data_from_row(f32::NAN, row_ptr, index, error_ptr)
}

/**
Tries to retrieve an f64 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_f64(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> f64 {
    get_data_from_row(f64::NAN, row_ptr, index, error_ptr)
}

/**
Tries to retrieve an FFIString from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_str<'a, 'b>(
    row_ptr: &'a Row,
    index: FFIString<'_>,
    error_ptr: &'b mut Error,
) -> FFIString<'a> {
    get_data_from_row("", row_ptr, index, error_ptr)
}

/**
Tries to retrieve a FFIDate from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
*/
#[no_mangle]
pub extern "C" fn rorm_row_get_date(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIDate {
    get_data_from_row(chrono::NaiveDate::MAX, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a FFITime from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
*/
#[no_mangle]
pub extern "C" fn rorm_row_get_time(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFITime {
    get_data_from_row(
        chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        row_ptr,
        index,
        error_ptr,
    )
}

/**
Tries to retrieve a FFIDateTime from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
*/
#[no_mangle]
pub extern "C" fn rorm_row_get_datetime(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIDateTime {
    get_data_from_row(
        chrono::NaiveDateTime::new(
            chrono::NaiveDate::MAX,
            chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        ),
        row_ptr,
        index,
        error_ptr,
    )
}

/**
Tries to retrieve a nullable bool from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_bool(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<bool> {
    get_data_from_row(None, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a nullable i64 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_i64(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<i64> {
    get_data_from_row(None, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a nullable i32 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_i32(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<i32> {
    get_data_from_row(None, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a nullable i16 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_i16(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<i16> {
    get_data_from_row(None, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a nullable FFISlice of a u8 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_binary<'a>(
    row_ptr: &'a Row,
    index: FFIString<'a>,
    error_ptr: &mut Error,
) -> FFIOption<FFISlice<'a, u8>> {
    match get_data_from_row::<Option<&[u8]>, Option<&[u8]>>(None, row_ptr, index, error_ptr) {
        None => FFIOption::None,
        Some(v) => FFIOption::Some(v.into()),
    }
}

/**
Tries to retrieve a nullable f32 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_f32(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<f32> {
    get_data_from_row(None, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a nullable f64 from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_f64(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<f64> {
    get_data_from_row(None, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a nullable FFIString from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_str<'a, 'b>(
    row_ptr: &'a Row,
    index: FFIString<'_>,
    error_ptr: &'b mut Error,
) -> FFIOption<FFIString<'a>> {
    match get_data_from_row::<Option<&str>, Option<&str>>(None, row_ptr, index, error_ptr) {
        None => FFIOption::None,
        Some(v) => match v.try_into() {
            Err(_) => {
                *error_ptr = Error::InvalidStringError;
                FFIOption::None
            }
            Ok(v) => FFIOption::Some(v),
        },
    }
}

/**
Tries to retrieve a nullable FFIDate from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_date(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<FFIDate> {
    get_data_from_row::<Option<chrono::NaiveDate>, _>(None, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a nullable FFITime from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_time(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<FFITime> {
    get_data_from_row::<Option<chrono::NaiveTime>, _>(None, row_ptr, index, error_ptr)
}

/**
Tries to retrieve a nullable FFIDate from the given row pointer.

**Parameter**:
- `row_ptr`: Pointer to a row.
- `index`: Name of the column to retrieve from the row.
- `error_ptr`: Pointer to an [Error]. Gets only written to if an error occurs.

This function is called completely synchronously.
 */
#[no_mangle]
pub extern "C" fn rorm_row_get_null_datetime(
    row_ptr: &Row,
    index: FFIString<'_>,
    error_ptr: &mut Error,
) -> FFIOption<FFIDateTime> {
    get_data_from_row::<Option<chrono::NaiveDateTime>, _>(None, row_ptr, index, error_ptr)
}
