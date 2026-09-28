//! Заглушка корневого бинаря.
//!
//! Основной entrypoint — `crates/worker` (бинарь `worker`).
//! Этот файл существует только чтобы workspace оставался консистентным.

fn main() {
    eprintln!("This is a library workspace. Run the `worker` binary from `crates/worker`.");
    std::process::exit(1);
}