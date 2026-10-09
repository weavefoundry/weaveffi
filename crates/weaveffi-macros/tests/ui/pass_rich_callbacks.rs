//! Callback methods return any family, may throw a declared domain (whose
//! typed errors arrive typed), and a callback parameter may be optional.

use std::sync::Arc;

#[weaveffi::module]
mod rich {
    use std::sync::Arc;

    use weaveffi::ForeignError;

    #[weaveffi::error]
    #[repr(i32)]
    #[derive(Debug)]
    pub enum LookupError {
        #[weaveffi(message = "missing {key}")]
        Missing { key: String } = 1,
        #[weaveffi(message = "{message}")]
        Other { message: String } = 2,
    }

    impl From<ForeignError> for LookupError {
        fn from(e: ForeignError) -> Self {
            Self::Other { message: e.message }
        }
    }

    #[weaveffi::record]
    pub struct Card {
        pub name: String,
    }

    #[weaveffi::interface]
    pub struct Token;

    impl Token {
        pub fn new() -> Self {
            Token
        }
    }

    #[weaveffi::callback_interface]
    pub trait Source: Send + Sync {
        fn name(&self) -> Result<String, ForeignError>;
        fn blob(&self) -> Result<Vec<u8>, ForeignError>;
        fn card(&self) -> Result<Card, ForeignError>;
        fn token(&self) -> Result<Arc<Token>, ForeignError>;
        fn maybe(&self) -> Result<Option<Arc<Token>>, ForeignError>;
        fn lookup(&self, key: &str) -> Result<i64, LookupError>;
    }

    #[weaveffi::export]
    pub fn read(source: Option<Arc<dyn Source>>) -> Result<String, ForeignError> {
        match source {
            Some(s) => s.name(),
            None => Ok(String::new()),
        }
    }

    #[weaveffi::export]
    pub fn missing_key(source: Arc<dyn Source>) -> Option<String> {
        match source.lookup("k").err()? {
            LookupError::Missing { key } => Some(key),
            LookupError::Other { .. } => None,
        }
    }
}

weaveffi::export_runtime!();

fn main() {
    let _ = Arc::new(rich::Token::new());
}
