use zbus::proxy;

// This macro builds an struct PowerDaemonProxy automatically
#[proxy(
    interface = "dev.cirodev.hpd.PowerDaemon1",
    default_service = "dev.cirodev.hpd.PowerDaemon1",
    default_path = "/dev/cirodev/hpd/PowerDaemon1"
)]
trait PowerDaemon {
    async fn set_spl(&self, watts: u32) -> zbus::Result<()>;
    
    #[zbus(property)]
    fn current_spl(&self) -> zbus::Result<u32>;
}