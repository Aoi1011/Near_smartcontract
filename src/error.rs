use std::{error::Error, fmt::Display};

use url::ParseError;

#[derive(Debug)]
pub enum PriceServiceError {
    /// The given URL is invalid
    BadUrl(ParseError),

    /// The Errors may occur when processing a Request
    Failed(reqwest::Error),
}

impl Error for PriceServiceError {
    // fn description(&self) -> &str {
    //     match *self {
    //         PriceServiceError::BadUrl(..) => "bad url provided",
    //         PriceServiceError::Failed(..) => "could not be reached",
    //     }
    // }
}

impl Display for PriceServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            PriceServiceError::BadUrl(ref e) => write!(f, "bad url provided {}", e),
            PriceServiceError::Failed(ref e) => write!(f, "could not be reached {}", e),
        }
    }
}
