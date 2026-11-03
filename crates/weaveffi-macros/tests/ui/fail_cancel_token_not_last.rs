//! A cancellable function takes its `CancelToken` last.

#[weaveffi::export]
pub async fn wait(cancel: weaveffi::CancelToken, ms: u64) -> u64 {
    let _ = cancel;
    ms
}

weaveffi::export_runtime!();

fn main() {}
