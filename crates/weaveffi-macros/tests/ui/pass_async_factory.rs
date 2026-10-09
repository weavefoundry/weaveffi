//! An `async fn` associated function returning the interface is an async
//! static factory (constructors are synchronous), in every return spelling.
#![deny(unsafe_code)]

#[weaveffi::module]
mod store {
    use std::sync::Arc;

    #[weaveffi::error]
    #[repr(i32)]
    pub enum StoreError {
        /// Empty path
        InvalidPath = 1,
    }

    impl std::fmt::Display for StoreError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("invalid path")
        }
    }

    #[weaveffi::interface]
    pub struct Store {
        path: String,
    }

    impl Store {
        pub fn new(path: String) -> Self {
            Self { path }
        }

        pub async fn open_async(path: String) -> Result<Arc<Self>, StoreError> {
            if path.is_empty() {
                Err(StoreError::InvalidPath)
            } else {
                Ok(Arc::new(Self { path }))
            }
        }

        #[weaveffi::cancellable]
        pub async fn open_later(path: String, cancel: weaveffi::CancelToken) -> Self {
            let _ = cancel;
            Self { path }
        }

        pub async fn reopen(path: String) -> Store {
            Self { path }
        }

        pub fn path(&self) -> String {
            self.path.clone()
        }
    }
}

fn main() {
    let _ = store::Store::new("x".into()).path();
}
