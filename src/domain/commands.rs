use super::remote::SyncPolicy;

#[derive(Debug)]
pub enum SchedulerCommand {
    SyncNow,
    UpdatePolicy(SyncPolicy),
    Shutdown,
}
