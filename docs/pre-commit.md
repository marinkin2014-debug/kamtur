# Pre-commit hooks

Локальные git-хуки для kamtur. Проверяют код **до коммита и до push**,
чтобы ловить проблемы за секунды, а не за минуты в CI.

## Что проверяется

### Стадия `pre-commit` (быстро, <10 сек в горячем кэше)

| Хук | Что делает | Время |
|-----|-----------|-------|
| `trailing-whitespace` | убирает хвостовые пробелы (кроме `<br>` в markdown) | <1 сек |
| `end-of-file-fixer` | newline в конце файла | <1 сек |
| `mixed-line-ending` | приводит к LF | <1 сек |
| `check-yaml` | парсит YAML | <1 сек |
| `check-toml` | парсит TOML | <1 сек |
| `check-merge-conflict` | ловит маркеры `<<<<<<<` | <1 сек |
| `check-case-conflict` | ловит файлы, различающиеся регистром | <1 сек |
| `check-added-large-files` | файлы ≤1 MB | <1 сек |
| `detect-private-key` | PEM/SSH-ключи | <1 сек |
| **`cargo-fmt`** | `cargo fmt --all -- --check` | <1 сек |
| **`cargo-clippy`** | `cargo clippy --workspace --all-targets -- -D warnings` | 1-3 сек (кэш) / минуты (холодный) |

### Стадия `pre-push` (медленнее, минуты)

| Хук | Что делает |
|-----|-----------|
| **`cargo-test`** | `cargo test --workspace` (требует `TEST_DATABASE_URL`) |

## Установка

### Windows

```powershell
.\scripts\install-hooks.ps1
