use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL")?;

    println!("connecting to: {}", mask_url(&url));

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .context("connect")?;

    let (cur, sess): (String, String) = sqlx::query_as("SELECT current_user, session_user")
        .fetch_one(&pool)
        .await?;
    println!("current_user = {cur}");
    println!("session_user = {sess}");

    let (createdb, createrole, super_): (bool, bool, bool) = sqlx::query_as(
        "SELECT rolcreatedb, rolcreaterole, rolsuper
         FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&pool)
    .await?;
    println!(
        "can_create_databases = {createdb}  can_create_roles = {createrole}  is_superuser = {super_}"
    );

    let dbs: Vec<String> = sqlx::query_scalar(
        "SELECT datname FROM pg_database
         WHERE datistemplate = false AND datname NOT LIKE 'template%'
         ORDER BY datname",
    )
    .fetch_all(&pool)
    .await?;
    println!("existing databases:");
    for db in &dbs {
        println!("  - {db}");
    }

    Ok(())
}

fn mask_url(url: &str) -> String {
    if let Some(at) = url.find('@') {
        if let Some(scheme_end) = url.find("://") {
            let scheme = &url[..scheme_end + 3];
            return format!("{}***{}", scheme, &url[at..]);
        }
    }
    url.to_string()
}
