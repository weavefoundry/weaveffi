//! A module that touches every construct the macro supports must expand
//! and compile cleanly.
#![deny(unsafe_code)]

use std::sync::Arc;

#[weaveffi::module]
mod surface {
    use std::sync::Arc;

    #[weaveffi::error]
    #[repr(i32)]
    pub enum Failure {
        /// Not found
        NotFound = 1,
        /// Payload variant
        Detailed { code: i32, note: String } = 2,
    }

    #[weaveffi::enumeration]
    #[repr(i32)]
    pub enum Mode {
        Fast = 0,
        Safe = 1,
    }

    #[weaveffi::record]
    pub struct Item {
        pub id: i64,
        pub name: String,
        pub tags: Vec<String>,
        pub mode: Mode,
        pub owner: Option<Arc<Widget>>,
    }

    #[weaveffi::callback_interface]
    pub trait Observer: Send + Sync {
        fn on_item(&self, item: &Item, widget: Arc<Widget>);
        fn should_continue(&self, n: i32) -> bool;
        fn pick(&self) -> Mode;
    }

    #[weaveffi::interface]
    pub struct Widget {
        id: i64,
    }

    impl Widget {
        pub fn new(id: i64) -> Self {
            Self { id }
        }
        pub fn id(&self) -> i64 {
            self.id
        }
        pub fn me(self: Arc<Self>) -> Arc<Self> {
            self
        }
        pub fn twin(&self) -> Option<Arc<Widget>> {
            None
        }
        pub fn poke(&self, n: i32) -> Result<i32, Failure> {
            if n < 0 {
                Err(Failure::NotFound)
            } else {
                Ok(n)
            }
        }
        pub async fn later(self: Arc<Self>) -> i64 {
            self.id
        }
        pub fn version() -> String {
            "1".into()
        }
    }

    #[weaveffi::export]
    pub fn watch(observer: Arc<dyn Observer>, widget: Arc<Widget>) -> bool {
        observer.on_item(
            &Item {
                id: 1,
                name: "x".into(),
                tags: vec![],
                mode: observer.pick(),
                owner: Some(widget.clone()),
            },
            widget,
        );
        observer.should_continue(1)
    }

    #[weaveffi::export]
    pub fn widgets(n: i32) -> Vec<Arc<Widget>> {
        (0..n).map(|i| Arc::new(Widget::new(i64::from(i)))).collect()
    }

    #[weaveffi::export]
    pub fn stream(items: Vec<Item>) -> weaveffi::Iter<Item> {
        weaveffi::Iter::new(items)
    }

    #[weaveffi::export]
    pub fn stream_widgets() -> weaveffi::Iter<Arc<Widget>> {
        weaveffi::Iter::new(vec![Arc::new(Widget::new(7))])
    }

    #[weaveffi::export]
    pub async fn fetch(id: i64) -> Result<Item, Failure> {
        Err(Failure::Detailed {
            code: 3,
            note: format!("{id}"),
        })
    }

    #[weaveffi::export]
    #[weaveffi::cancellable]
    pub async fn slow(bytes: Vec<u8>, cancel: weaveffi::CancelToken) -> Vec<u8> {
        let _ = cancel;
        bytes
    }

    #[weaveffi::export]
    pub fn maybe(text: Option<String>, w: Option<Arc<Widget>>) -> Option<Arc<Widget>> {
        let _ = text;
        w
    }
}

fn main() {
    let _ = Arc::new(surface::Widget::new(1));
}
