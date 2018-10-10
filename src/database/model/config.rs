use super::super::schema::configs;

#[derive(Debug, Queryable)]
pub struct Config {
  pub server_id: i64,
  pub moderator_role: Option<String>,
  pub category: Option<i64>,
}

#[derive(Debug, Insertable)]
#[table_name = "configs"]
pub struct NewConfig {
  pub server_id: i64,
  pub moderator_role: Option<String>,
  pub category: Option<i64>,
}
