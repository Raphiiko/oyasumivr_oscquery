#[macro_use]
extern crate lazy_static;

pub mod client;
pub mod models;
pub mod server;
use mdns_sd::ServiceDaemon;
pub use models::*;
use tokio::sync::Mutex;

lazy_static! {
    pub(crate) static ref MDNS_DAEMON: Mutex<Option<ServiceDaemon>> = Mutex::default();
}

pub(crate) async fn init_mdns_daemon() -> Result<(), Error> {
    // Stop if already initialized
    let mut mdns_daemon = MDNS_DAEMON.lock().await;
    if mdns_daemon.is_some() {
        return Ok(());
    }
    // Initialize MDNS daemon
    let daemon = match ServiceDaemon::new() {
        Ok(daemon) => daemon,
        Err(e) => {
            return Err(Error::InitError(OSCQueryInitError::MDNSDaemonInitFailed(e)));
        }
    };
    *mdns_daemon = Some(daemon);
    Ok(())
}
