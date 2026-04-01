mod dispatch;
mod epics;
mod tasks;
mod types;

#[cfg(test)]
mod tests;

pub use dispatch::handle_mcp;
