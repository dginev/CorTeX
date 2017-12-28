// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Dynamically inferred Diesel schema from an already initialized database
//! Run `diesel migration run` to initialize DB

infer_schema!("dotenv:DATABASE_URL");
