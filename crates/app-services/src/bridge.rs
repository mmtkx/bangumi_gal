use library_core::EventBus;
use tauri::{AppHandle, Emitter};

/// 前端订阅的事件通道名。主题放在信封的 `topic` 字段里，前端按主题分发。
pub const EVENT_CHANNEL: &str = "lagm://event";

/// 把主干道事件转发为 Tauri event。
///
/// 慢订阅者只丢自己的旧事件（broadcast 语义），不会阻塞后端业务流程。
pub fn spawn_event_bridge(app: AppHandle, bus: EventBus) {
    tauri::async_runtime::spawn(async move {
        let mut receiver = bus.subscribe();
        loop {
            match receiver.recv().await {
                Ok(event) => match event.to_envelope() {
                    Ok(envelope) => {
                        if let Err(err) = app.emit(EVENT_CHANNEL, envelope) {
                            tracing::warn!(error = %err, "向前端派发事件失败");
                        }
                    }
                    Err(err) => tracing::warn!(error = %err, "事件序列化失败"),
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "事件订阅落后，已跳过部分事件");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
