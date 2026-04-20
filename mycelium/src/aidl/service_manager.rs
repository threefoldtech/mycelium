//! Android 15 ServiceManager registration workaround.
//!
//! rsbinder 0.6 doesn't have a correct IServiceManager AIDL for Android 15
//! (API 35), which inserted `getService2` between `getService` and
//! `checkService`, shifting transaction codes. We generate our own bindings
//! from a corrected AIDL file.

include!(concat!(env!("OUT_DIR"), "/service_manager_a15.rs"));

use android::os::IServiceManager::{BpServiceManager, IServiceManager};
use rsbinder::{ProcessState, Proxy, SIBinder};

const DUMP_FLAG_PRIORITY_DEFAULT: i32 = 1 << 3;

/// Register a Binder service with Android's ServiceManager.
pub fn add_service(name: &str, binder: SIBinder) -> Result<(), rsbinder::status::Status> {
    let context = ProcessState::as_self()
        .context_object()
        .expect("failed to get ServiceManager binder");
    let sm = BpServiceManager::from_binder(context).expect("failed to create BpServiceManager");
    sm.addService(name, &binder, false, DUMP_FLAG_PRIORITY_DEFAULT)
}
