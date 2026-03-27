use std::error::Error;
use std::ffi::NulError;
use std::fmt::{Display, Formatter};
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum StorageError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    InvalidPath(PathBuf),
    PathResolution(String),
    Sqlite {
        code: i32,
        message: String,
    },
    CString(NulError),
}

impl StorageError {
    pub fn io(path: PathBuf, source: io::Error) -> Self {
        Self::Io { path, source }
    }

    pub fn sqlite(code: i32, message: String) -> Self {
        Self::Sqlite { code, message }
    }
}

impl Display for StorageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "io error at {}: {}", path.display(), source)
            }
            Self::InvalidPath(path) => write!(f, "invalid filesystem path: {}", path.display()),
            Self::PathResolution(message) => write!(f, "path resolution failed: {}", message),
            Self::Sqlite { code, message } => {
                write!(f, "sqlite error {}: {}", code, message)
            }
            Self::CString(source) => write!(f, "c string conversion failed: {}", source),
        }
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::CString(source) => Some(source),
            Self::InvalidPath(_) | Self::PathResolution(_) | Self::Sqlite { .. } => None,
        }
    }
}

impl From<NulError> for StorageError {
    fn from(source: NulError) -> Self {
        Self::CString(source)
    }
}
