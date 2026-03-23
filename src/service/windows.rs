#[cfg(windows)]
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

#[cfg(windows)]
const SERVICE_NAME: &str = "WHCanRCAssistedListening";
#[cfg(windows)]
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

#[cfg(windows)]
define_windows_service!(ffi_service_main, service_main);

#[cfg(windows)]
pub fn run_as_service() -> anyhow::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

#[cfg(windows)]
fn service_main(arguments: Vec<std::ffi::OsString>) {
    if let Err(e) = run_service(arguments) {
        tracing::error!("Service failed: {}", e);
    }
}

#[cfg(windows)]
fn run_service(_arguments: Vec<std::ffi::OsString>) -> anyhow::Result<()> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let shutdown_tx = std::sync::Mutex::new(Some(shutdown_tx));

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop => {
                if let Ok(mut tx) = shutdown_tx.lock() {
                    if let Some(tx) = tx.take() {
                        let _ = tx.send(());
                    }
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;

    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: std::time::Duration::default(),
        process_id: None,
    })?;

    // Run the main application in a tokio runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let _ = shutdown_rx.await;
    });

    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: std::time::Duration::default(),
        process_id: None,
    })?;

    Ok(())
}

// Provide a stub when not on Windows so the module compiles
#[cfg(not(windows))]
pub fn run_as_service() -> anyhow::Result<()> {
    anyhow::bail!("Windows service support is only available on Windows")
}
