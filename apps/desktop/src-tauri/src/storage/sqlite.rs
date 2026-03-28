use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;
use std::ptr::{self, NonNull};

use super::StorageError;

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x0001_0000;

type Sqlite3 = c_void;
type Sqlite3Statement = c_void;
type SqliteDestructor = unsafe extern "C" fn(*mut c_void);

#[link(name = "sqlite3")]
unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        db: *mut *mut Sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut Sqlite3) -> *const c_char;
    fn sqlite3_exec(
        db: *mut Sqlite3,
        sql: *const c_char,
        callback: Option<unsafe extern "C" fn()>,
        context: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(value: *mut c_void);
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        sql: *const c_char,
        byte_count: c_int,
        statement: *mut *mut Sqlite3Statement,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_int64(statement: *mut Sqlite3Statement, index: c_int, value: i64) -> c_int;
    fn sqlite3_bind_null(statement: *mut Sqlite3Statement, index: c_int) -> c_int;
    fn sqlite3_bind_text(
        statement: *mut Sqlite3Statement,
        index: c_int,
        value: *const c_char,
        byte_count: c_int,
        destructor: Option<SqliteDestructor>,
    ) -> c_int;
    fn sqlite3_step(statement: *mut Sqlite3Statement) -> c_int;
    fn sqlite3_finalize(statement: *mut Sqlite3Statement) -> c_int;
    fn sqlite3_last_insert_rowid(db: *mut Sqlite3) -> i64;
    fn sqlite3_column_int64(statement: *mut Sqlite3Statement, column: c_int) -> i64;
    fn sqlite3_column_text(statement: *mut Sqlite3Statement, column: c_int) -> *const c_char;
    fn sqlite3_column_type(statement: *mut Sqlite3Statement, column: c_int) -> c_int;
}

const SQLITE_NULL: c_int = 5;

fn sqlite_transient() -> SqliteDestructor {
    unsafe { std::mem::transmute::<isize, SqliteDestructor>(-1_isize) }
}

pub struct Connection {
    raw: NonNull<Sqlite3>,
}

impl Connection {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let path_str = path
            .to_str()
            .ok_or_else(|| StorageError::InvalidPath(path.to_path_buf()))?;
        let filename = CString::new(path_str)?;
        let mut raw_db = ptr::null_mut();
        let rc = unsafe {
            sqlite3_open_v2(
                filename.as_ptr(),
                &mut raw_db,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX,
                ptr::null(),
            )
        };

        let raw = NonNull::new(raw_db).ok_or_else(|| {
            StorageError::sqlite(rc, "sqlite3_open_v2 returned a null connection".to_string())
        })?;

        let connection = Self { raw };
        if rc != SQLITE_OK {
            return Err(connection.last_error(rc));
        }

        Ok(connection)
    }

    pub fn exec_batch(&self, sql: &str) -> Result<(), StorageError> {
        let sql = CString::new(sql)?;
        let mut error_message = ptr::null_mut();
        let rc = unsafe {
            sqlite3_exec(
                self.raw.as_ptr(),
                sql.as_ptr(),
                None,
                ptr::null_mut(),
                &mut error_message,
            )
        };

        if rc != SQLITE_OK {
            let message = if error_message.is_null() {
                self.error_message()
            } else {
                let message = unsafe { CStr::from_ptr(error_message) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { sqlite3_free(error_message.cast()) };
                message
            };
            return Err(StorageError::sqlite(rc, message));
        }

        Ok(())
    }

    pub fn prepare<'db>(&'db self, sql: &str) -> Result<Statement<'db>, StorageError> {
        let sql = CString::new(sql)?;
        let mut raw_statement = ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(
                self.raw.as_ptr(),
                sql.as_ptr(),
                -1,
                &mut raw_statement,
                ptr::null_mut(),
            )
        };

        let raw = NonNull::new(raw_statement).ok_or_else(|| {
            StorageError::sqlite(rc, "sqlite3_prepare_v2 returned a null statement".to_string())
        })?;

        if rc != SQLITE_OK {
            return Err(self.last_error(rc));
        }

        Ok(Statement { db: self, raw })
    }

    pub fn last_insert_rowid(&self) -> i64 {
        unsafe { sqlite3_last_insert_rowid(self.raw.as_ptr()) }
    }

    fn error_message(&self) -> String {
        unsafe { CStr::from_ptr(sqlite3_errmsg(self.raw.as_ptr())) }
            .to_string_lossy()
            .into_owned()
    }

    fn last_error(&self, code: c_int) -> StorageError {
        StorageError::sqlite(code, self.error_message())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            sqlite3_close(self.raw.as_ptr());
        }
    }
}

pub struct Statement<'db> {
    db: &'db Connection,
    raw: NonNull<Sqlite3Statement>,
}

impl<'db> Statement<'db> {
    pub fn bind_int64(&mut self, index: i32, value: i64) -> Result<(), StorageError> {
        let rc = unsafe { sqlite3_bind_int64(self.raw.as_ptr(), index, value) };
        if rc != SQLITE_OK {
            return Err(self.db.last_error(rc));
        }
        Ok(())
    }

    pub fn bind_null(&mut self, index: i32) -> Result<(), StorageError> {
        let rc = unsafe { sqlite3_bind_null(self.raw.as_ptr(), index) };
        if rc != SQLITE_OK {
            return Err(self.db.last_error(rc));
        }
        Ok(())
    }

    pub fn bind_text(&mut self, index: i32, value: &str) -> Result<(), StorageError> {
        let value = CString::new(value)?;
        let rc = unsafe {
            sqlite3_bind_text(
                self.raw.as_ptr(),
                index,
                value.as_ptr(),
                -1,
                Some(sqlite_transient()),
            )
        };
        if rc != SQLITE_OK {
            return Err(self.db.last_error(rc));
        }
        Ok(())
    }

    pub fn execute(self) -> Result<(), StorageError> {
        let rc = unsafe { sqlite3_step(self.raw.as_ptr()) };
        if rc != SQLITE_DONE {
            return Err(self.db.last_error(rc));
        }
        Ok(())
    }

    pub fn query_int64(self) -> Result<Option<i64>, StorageError> {
        let rc = unsafe { sqlite3_step(self.raw.as_ptr()) };
        match rc {
            SQLITE_ROW => Ok(Some(unsafe { sqlite3_column_int64(self.raw.as_ptr(), 0) })),
            SQLITE_DONE => Ok(None),
            _ => Err(self.db.last_error(rc)),
        }
    }

    pub fn step(&mut self) -> Result<bool, StorageError> {
        let rc = unsafe { sqlite3_step(self.raw.as_ptr()) };
        match rc {
            SQLITE_ROW => Ok(true),
            SQLITE_DONE => Ok(false),
            _ => Err(self.db.last_error(rc)),
        }
    }

    pub fn column_int64(&self, column: i32) -> i64 {
        unsafe { sqlite3_column_int64(self.raw.as_ptr(), column) }
    }

    pub fn column_is_null(&self, column: i32) -> bool {
        unsafe { sqlite3_column_type(self.raw.as_ptr(), column) == SQLITE_NULL }
    }

    pub fn column_text(&self, column: i32) -> Result<Option<String>, StorageError> {
        let value_type = unsafe { sqlite3_column_type(self.raw.as_ptr(), column) };
        if value_type == SQLITE_NULL {
            return Ok(None);
        }

        let raw = unsafe { sqlite3_column_text(self.raw.as_ptr(), column) };
        if raw.is_null() {
            return Ok(None);
        }

        Ok(Some(
            unsafe { CStr::from_ptr(raw) }
                .to_string_lossy()
                .into_owned(),
        ))
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        unsafe {
            sqlite3_finalize(self.raw.as_ptr());
        }
    }
}
