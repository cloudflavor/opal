mod protocol;
mod resources;
mod server;
mod tools;
mod uri;

pub use server::serve_stdio;

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
