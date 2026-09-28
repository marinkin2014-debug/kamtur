use domain::errors::RepositoryError;

#[inline]
pub(super) fn tx_err(e: sqlx::Error) -> RepositoryError {
    RepositoryError::Transaction(e.to_string())
}
