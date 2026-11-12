//! CLAP compatibility names backed by the shared application-main executor.

pub use crate::application_main::{
    ApplicationControlJob as ClapControlJob, ApplicationMainExecutor as ClapMainExecutor,
    ApplicationMainQueue as ClapMainQueue, ApplicationMainTask as ClapMainTask,
    ApplicationOfflineMainScope as ClapOfflineMainScope, offline_main_is_active,
    service_offline_main,
};
pub(crate) use crate::application_main::{MainService, MainThreadToken};
