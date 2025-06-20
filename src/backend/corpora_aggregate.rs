use crate::models::Corpus;
use crate::schema::corpora;
use diesel::*;

pub fn list_corpora(connection: &mut PgConnection) -> Vec<Corpus> {
  corpora::table
    .order(corpora::name.asc())
    .load(connection)
    .unwrap_or_default()
}
