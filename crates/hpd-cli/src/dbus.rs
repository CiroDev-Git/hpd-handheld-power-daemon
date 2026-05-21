use zbus::proxy;

// This macro builds an struct PowerDaemonProxy automatically
#[proxy(
    interface = "dev.cirodev.hpd.PowerDaemon1",
    default_service = "dev.cirodev.hpd.PowerDaemon1",
    default_path = "/dev/cirodev/hpd/PowerDaemon1"
)]
trait PowerDaemon {
    async fn set_preset(&self, preset_name: &str) -> zbus::Result<()>;
    async fn set_spl(&self, watts: u32) -> zbus::Result<()>;
    async fn set_charge_threshold(&self, threshold: u8) -> zbus::Result<()>;

    #[zbus(property)]
    fn current_spl(&self) -> zbus::Result<u32>;
    
    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;
    
    #[zbus(property)]
    fn charge_end_threshold(&self) -> zbus::Result<u8>;
}