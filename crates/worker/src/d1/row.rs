//! Positional reader for rows from `raw()`: each row is a JS array in SELECT order. Avoids
//! serde for a handful of column types.

use plumblog_model::Result;
use worker::js_sys::{Array, BigInt, Number};
use worker::wasm_bindgen::{JsCast, JsValue};

use super::storage;

pub(crate) struct Row {
    name: &'static str,
    cells: Array,
    next: u32,
}

impl Row {
    pub(super) fn new(name: &'static str, value: JsValue) -> Result<Self> {
        let cells = value
            .dyn_into::<Array>()
            .map_err(|v| storage(name, &format!("row is not an array: {v:?}")))?;
        Ok(Self {
            name,
            cells,
            next: 0,
        })
    }

    fn take(&mut self) -> Result<JsValue> {
        if self.next >= self.cells.length() {
            return Err(storage(
                self.name,
                &format!("row has {} columns, wanted more", self.cells.length()),
            ));
        }
        let v = self.cells.get(self.next);
        self.next += 1;
        Ok(v)
    }

    fn bad(&self, wanted: &str, v: &JsValue) -> plumblog_model::Error {
        storage(
            self.name,
            &format!("column {} is not {wanted}: {v:?}", self.next - 1),
        )
    }

    pub fn opt_int(&mut self) -> Result<Option<i64>> {
        let v = self.take()?;
        if v.is_null() || v.is_undefined() {
            return Ok(None);
        }
        // Integers arrive as JS numbers; every value we store is below 2^53, so a number that
        // is not a safe integer means the row is not what we think it is.
        if !Number::is_safe_integer(&v) {
            return Err(self.bad("an integer", &v));
        }
        let big = BigInt::new(&v).map_err(|_| self.bad("an integer", &v))?;
        i64::try_from(big)
            .map(Some)
            .map_err(|_| self.bad("an integer", &v))
    }

    pub fn int(&mut self) -> Result<i64> {
        self.opt_int()?.ok_or_else(|| {
            storage(
                self.name,
                &format!("column {} is NULL, wanted an integer", self.next - 1),
            )
        })
    }

    pub fn uint(&mut self) -> Result<u64> {
        let n = self.int()?;
        u64::try_from(n)
            .map_err(|_| self.bad("a non-negative integer", &JsValue::from_str(&n.to_string())))
    }

    pub fn flag(&mut self) -> Result<bool> {
        match self.int()? {
            0 => Ok(false),
            1 => Ok(true),
            n => Err(self.bad("0 or 1", &JsValue::from_str(&n.to_string()))),
        }
    }

    pub fn opt_text(&mut self) -> Result<Option<String>> {
        let v = self.take()?;
        if v.is_null() || v.is_undefined() {
            return Ok(None);
        }
        v.as_string().map(Some).ok_or_else(|| self.bad("text", &v))
    }

    pub fn text(&mut self) -> Result<String> {
        self.opt_text()?.ok_or_else(|| {
            storage(
                self.name,
                &format!("column {} is NULL, wanted text", self.next - 1),
            )
        })
    }

    /// A stored code parsed with `parse`; an unknown code is a storage error (a row written
    /// by a newer version, or corruption), never a silent default.
    pub fn code<T>(&mut self, what: &str, parse: impl Fn(&str) -> Option<T>) -> Result<T> {
        let s = self.text()?;
        parse(&s).ok_or_else(|| storage(self.name, &format!("unknown {what} code {s:?}")))
    }

    pub fn opt_code<T>(
        &mut self,
        what: &str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Result<Option<T>> {
        match self.opt_text()? {
            None => Ok(None),
            Some(s) => parse(&s)
                .map(Some)
                .ok_or_else(|| storage(self.name, &format!("unknown {what} code {s:?}"))),
        }
    }
}
