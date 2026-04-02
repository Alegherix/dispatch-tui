mod dispatch;
mod epics;
mod tasks;
mod types;
mod validation;

#[cfg(test)]
mod tests;

pub use dispatch::handle_mcp;
