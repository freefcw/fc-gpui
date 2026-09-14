use anyhow::Result;
use objc2::rc::Retained;
use objc2::runtime::AnyClass;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

fn main_app_service() -> Result<Retained<SMAppService>> {
    if AnyClass::get(c"SMAppService").is_none() {
        return Err(anyhow::anyhow!(
            "SMAppService not available (requires macOS 13+)"
        ));
    }
    Ok(unsafe { SMAppService::mainAppService() })
}

pub fn set_auto_launch(_app_id: &str, enabled: bool) -> Result<()> {
    let service = main_app_service()?;
    if enabled {
        unsafe { service.registerAndReturnError() }
            .map_err(|_| anyhow::anyhow!("Failed to register auto-launch"))?;
    } else {
        unsafe { service.unregisterAndReturnError() }
            .map_err(|_| anyhow::anyhow!("Failed to unregister auto-launch"))?;
    }
    Ok(())
}

pub fn is_auto_launch_enabled(_app_id: &str) -> bool {
    let Ok(service) = main_app_service() else {
        return false;
    };
    unsafe { service.status() == SMAppServiceStatus::Enabled }
}
