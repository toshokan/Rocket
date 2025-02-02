//! Automatic JSON (de)serialization support.
//!
//! See [`Json`](Json) for details.
//!
//! # Enabling
//!
//! This module is only available when the `json` feature is enabled. Enable it
//! in `Cargo.toml` as follows:
//!
//! ```toml
//! [dependencies.rocket]
//! version = "0.5.0-dev"
//! features = ["json"]
//! ```
//!
//! # Testing
//!
//! The [`LocalRequest`] and [`LocalResponse`] types provide [`json()`] and
//! [`into_json()`] methods to create a request with serialized JSON and
//! deserialize a response as JSON, respectively.
//!
//! [`LocalRequest`]: crate::local::blocking::LocalRequest
//! [`LocalResponse`]: crate::local::blocking::LocalResponse
//! [`json()`]: crate::local::blocking::LocalRequest::json()
//! [`into_json()`]: crate::local::blocking::LocalResponse::into_json()

use std::io;
use std::ops::{Deref, DerefMut};

use crate::request::{Request, local_cache};
use crate::data::{Limits, Data, FromData, Outcome};
use crate::response::{self, Responder, content};
use crate::http::Status;
use crate::form::prelude as form;

use serde::{Serialize, Deserialize};

#[doc(inline)]
pub use serde_json::{from_str, from_slice};

#[doc(hidden)]
pub use serde_json;

/// The JSON guard: easily consume and return JSON.
///
/// ## Receiving JSON
///
/// `Json` is both a data guard and a form guard.
///
/// ### Data Guard
///
/// To parse request body data as JSON , add a `data` route argument with a
/// target type of `Json<T>`, where `T` is some type you'd like to parse from
/// JSON. `T` must implement [`serde::Deserialize`].
///
/// ```rust
/// # #[macro_use] extern crate rocket;
/// # type User = usize;
/// use rocket::serde::json::Json;
///
/// #[post("/user", format = "json", data = "<user>")]
/// fn new_user(user: Json<User>) {
///     /* ... */
/// }
/// ```
///
/// You don't _need_ to use `format = "json"`, but it _may_ be what you want.
/// Using `format = json` means that any request that doesn't specify
/// "application/json" as its `Content-Type` header value will not be routed to
/// the handler.
///
/// ### Form Guard
///
/// `Json<T>`, as a form guard, accepts value and data fields and parses the
/// data as a `T`. Simple use `Json<T>`:
///
/// ```rust
/// # #[macro_use] extern crate rocket;
/// # type Metadata = usize;
/// use rocket::form::{Form, FromForm};
/// use rocket::serde::json::Json;
///
/// #[derive(FromForm)]
/// struct User<'r> {
///     name: &'r str,
///     metadata: Json<Metadata>
/// }
///
/// #[post("/user", data = "<form>")]
/// fn new_user(form: Form<User<'_>>) {
///     /* ... */
/// }
/// ```
///
/// ### Incoming Data Limits
///
/// The default size limit for incoming JSON data is 1MiB. Setting a limit
/// protects your application from denial of service (DoS) attacks and from
/// resource exhaustion through high memory consumption. The limit can be
/// increased by setting the `limits.json` configuration parameter. For
/// instance, to increase the JSON limit to 5MiB for all environments, you may
/// add the following to your `Rocket.toml`:
///
/// ```toml
/// [global.limits]
/// json = 5242880
/// ```
///
/// ## Sending JSON
///
/// If you're responding with JSON data, return a `Json<T>` type, where `T`
/// implements [`Serialize`] from [`serde`]. The content type of the response is
/// set to `application/json` automatically.
///
/// ```rust
/// # #[macro_use] extern crate rocket;
/// # type User = usize;
/// use rocket::serde::json::Json;
///
/// #[get("/users/<id>")]
/// fn user(id: usize) -> Json<User> {
///     let user_from_id = User::from(id);
///     /* ... */
///     Json(user_from_id)
/// }
/// ```
#[derive(Debug)]
pub struct Json<T>(pub T);

/// Error returned by the [`Json`] guard when JSON deserialization fails.
#[derive(Debug)]
pub enum Error<'a> {
    /// An I/O error occurred while reading the incoming request data.
    Io(io::Error),

    /// The client's data was received successfully but failed to parse as valid
    /// JSON or as the requested type. The `&str` value in `.0` is the raw data
    /// received from the user, while the `Error` in `.1` is the deserialization
    /// error from `serde`.
    Parse(&'a str, serde_json::error::Error),
}

impl<T> Json<T> {
    /// Consumes the JSON wrapper and returns the wrapped item.
    ///
    /// # Example
    /// ```rust
    /// # use rocket::serde::json::Json;
    /// let string = "Hello".to_string();
    /// let my_json = Json(string);
    /// assert_eq!(my_json.into_inner(), "Hello".to_string());
    /// ```
    #[inline(always)]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<'r, T: Deserialize<'r>> Json<T> {
    fn from_str(s: &'r str) -> Result<Self, Error<'r>> {
        serde_json::from_str(s).map(Json).map_err(|e| Error::Parse(s, e))
    }

    async fn from_data(req: &'r Request<'_>, data: Data) -> Result<Self, Error<'r>> {
        let limit = req.limits().get("json").unwrap_or(Limits::JSON);
        let string = match data.open(limit).into_string().await {
            Ok(s) if s.is_complete() => s.into_inner(),
            Ok(_) => {
                let eof = io::ErrorKind::UnexpectedEof;
                return Err(Error::Io(io::Error::new(eof, "data limit exceeded")));
            },
            Err(e) => return Err(Error::Io(e)),
        };

        Self::from_str(local_cache!(req, string))
    }
}

#[crate::async_trait]
impl<'r, T: Deserialize<'r>> FromData<'r> for Json<T> {
    type Error = Error<'r>;

    async fn from_data(req: &'r Request<'_>, data: Data) -> Outcome<Self, Self::Error> {
        match Self::from_data(req, data).await {
            Ok(value) => Outcome::Success(value),
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                Outcome::Failure((Status::PayloadTooLarge, Error::Io(e)))
            },
            Err(Error::Parse(s, e)) if e.classify() == serde_json::error::Category::Data => {
                Outcome::Failure((Status::UnprocessableEntity, Error::Parse(s, e)))
            },
            Err(e) => Outcome::Failure((Status::BadRequest, e)),

        }
    }
}

/// Serializes the wrapped value into JSON. Returns a response with Content-Type
/// JSON and a fixed-size body with the serialized value. If serialization
/// fails, an `Err` of `Status::InternalServerError` is returned.
impl<'r, T: Serialize> Responder<'r, 'static> for Json<T> {
    fn respond_to(self, req: &'r Request<'_>) -> response::Result<'static> {
        let string = serde_json::to_string(&self.0)
            .map_err(|e| {
                error_!("JSON failed to serialize: {:?}", e);
                Status::InternalServerError
            })?;

        content::Json(string).respond_to(req)
    }
}

impl<T> From<T> for Json<T> {
    fn from(value: T) -> Self {
        Json(value)
    }
}

impl<T> Deref for Json<T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Json<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

impl From<Error<'_>> for form::Error<'_> {
    fn from(e: Error<'_>) -> Self {
        match e {
            Error::Io(e) => e.into(),
            Error::Parse(_, e) => form::Error::custom(e)
        }
    }
}

#[crate::async_trait]
impl<'v, T: Deserialize<'v> + Send> form::FromFormField<'v> for Json<T> {
    fn from_value(field: form::ValueField<'v>) -> Result<Self, form::Errors<'v>> {
        Ok(Self::from_str(field.value)?)
    }

    async fn from_data(f: form::DataField<'v, '_>) -> Result<Self, form::Errors<'v>> {
        Ok(Self::from_data(f.request, f.data).await?)
    }
}

/// An arbitrary JSON value.
///
/// This structure wraps `serde`'s [`Value`] type. Importantly, unlike `Value`,
/// this type implements [`Responder`], allowing a value of this type to be
/// returned directly from a handler.
///
/// [`Value`]: serde_json::value
/// [`Responder`]: crate::response::Responder
///
/// # `Responder`
///
/// The `Responder` implementation for `Value` serializes the represented
/// value into a JSON string and sets the string as the body of a fixed-sized
/// response with a `Content-Type` of `application/json`.
///
/// # Usage
///
/// A value of this type is constructed via the [`json!`](json) macro. The macro
/// and this type are typically used to construct JSON values in an ad-hoc
/// fashion during request handling. This looks something like:
///
/// ```rust
/// # #[macro_use] extern crate rocket;
/// use rocket::serde::json::{json, Value};
///
/// #[get("/json")]
/// fn get_json() -> Value {
///     json!({
///         "id": 83,
///         "values": [1, 2, 3, 4]
///     })
/// }
/// ```
#[doc(inline)]
pub use serde_json::Value;

/// Serializes the value into JSON. Returns a response with Content-Type JSON
/// and a fixed-size body with the serialized value.
impl<'r> Responder<'r, 'static> for Value {
    fn respond_to(self, req: &'r Request<'_>) -> response::Result<'static> {
        content::Json(self.to_string()).respond_to(req)
    }
}

crate::export! {
    /// A macro to create ad-hoc JSON serializable values using JSON syntax.
    ///
    /// The return type of a `json!` invocation is [`Value`](Value). A value
    /// created with this macro can be returned from a handler as follows:
    ///
    /// ```rust
    /// # #[macro_use] extern crate rocket;
    /// use rocket::serde::json::{json, Value};
    ///
    /// #[get("/json")]
    /// fn get_json() -> Value {
    ///     json!({
    ///         "key": "value",
    ///         "array": [1, 2, 3, 4]
    ///     })
    /// }
    /// ```
    ///
    /// The [`Responder`](crate::response::Responder) implementation for
    /// `Value` serializes the value into a JSON string and sets it as the body
    /// of the response with a `Content-Type` of `application/json`.
    ///
    /// # Examples
    ///
    /// Create a simple JSON object with two keys: `"username"` and `"id"`:
    ///
    /// ```rust
    /// use rocket::serde::json::json;
    ///
    /// let value = json!({
    ///     "username": "mjordan",
    ///     "id": 23
    /// });
    /// ```
    ///
    /// Create a more complex object with a nested object and array:
    ///
    /// ```rust
    /// # use rocket::serde::json::json;
    /// let value = json!({
    ///     "code": 200,
    ///     "success": true,
    ///     "payload": {
    ///         "features": ["serde", "json"],
    ///         "ids": [12, 121],
    ///     },
    /// });
    /// ```
    ///
    /// Variables or expressions can be interpolated into the JSON literal. Any type
    /// interpolated into an array element or object value must implement serde's
    /// `Serialize` trait, while any type interpolated into a object key must
    /// implement `Into<String>`.
    ///
    /// ```rust
    /// # use rocket::serde::json::json;
    /// let code = 200;
    /// let features = vec!["serde", "json"];
    ///
    /// let value = json!({
    ///    "code": code,
    ///    "success": code == 200,
    ///    "payload": {
    ///        features[0]: features[1]
    ///    }
    /// });
    /// ```
    ///
    /// Trailing commas are allowed inside both arrays and objects.
    ///
    /// ```rust
    /// # use rocket::serde::json::json;
    /// let value = json!([
    ///     "notice",
    ///     "the",
    ///     "trailing",
    ///     "comma -->",
    /// ]);
    /// ```
    macro_rules! json {
        ($($json:tt)+) => ($crate::serde::json::serde_json::json!($($json)*));
    }
}
