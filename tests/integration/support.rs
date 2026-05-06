use codexize::app_runtime::AppView;

pub fn drain_views(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppView>) -> Vec<AppView> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

