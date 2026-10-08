//! Callback methods return any family, may throw the domain in scope, and a
//! callback parameter may be optional.

use std::sync::Arc;

#[weaveffi::module]
mod rich {
    use std::sync::Arc;

    use weaveffi::ForeignError;

    #[weaveffi::error]
    #[repr(i32)]
    #[derive(Debug)]
    pub enum LookupError {
        Missing { key: String } = 1,
    }

    impl std::fmt::Display for LookupError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("missing")
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
        #[weaveffi::throws]
        fn lookup(&self, key: &str) -> Result<i64, ForeignError>;
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
        match source.lookup("k").err()?.domain::<LookupError>()? {
            LookupError::Missing { key } => Some(key),
        }
    }
}

fn main() {
    let _ = Arc::new(rich::Token::new());
}
