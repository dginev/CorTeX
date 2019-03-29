use diesel::pg::PgConnection;
use diesel::*;
use crate::schema::corpora;
use crate::models::Corpus;

pub fn list_corpora(connection: &PgConnection) -> Vec<Corpus> {
  corpora::table
    .order(corpora::name.asc())
    .load(connection)
    .unwrap_or_default()
}