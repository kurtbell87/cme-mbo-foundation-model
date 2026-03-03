//! Raw C FFI wrappers for XGBoost training via libxgboost.
//!
//! Thin safe wrappers around the 8 C API functions needed for training:
//! DMatrix creation, Booster creation, parameter setting, training loop,
//! and prediction with ntree_limit.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr;

type BstUlong = u64;
type DMatrixHandle = *mut c_void;
type BoosterHandle = *mut c_void;

extern "C" {
    fn XGDMatrixCreateFromMat(
        data: *const f32,
        nrow: BstUlong,
        ncol: BstUlong,
        missing: f32,
        out: *mut DMatrixHandle,
    ) -> c_int;

    fn XGDMatrixSetFloatInfo(
        handle: DMatrixHandle,
        field: *const c_char,
        array: *const f32,
        len: BstUlong,
    ) -> c_int;

    fn XGDMatrixFree(handle: DMatrixHandle) -> c_int;

    fn XGBoosterCreate(
        dmats: *const DMatrixHandle,
        len: BstUlong,
        out: *mut BoosterHandle,
    ) -> c_int;

    fn XGBoosterFree(handle: BoosterHandle) -> c_int;

    fn XGBoosterSetParam(
        handle: BoosterHandle,
        name: *const c_char,
        value: *const c_char,
    ) -> c_int;

    fn XGBoosterUpdateOneIter(
        handle: BoosterHandle,
        iter: c_int,
        dtrain: DMatrixHandle,
    ) -> c_int;

    fn XGBoosterPredict(
        handle: BoosterHandle,
        dmat: DMatrixHandle,
        option_mask: c_int,
        ntree_limit: c_uint,
        out_len: *mut BstUlong,
        out_result: *mut *const f32,
    ) -> c_int;
}

// ---------------------------------------------------------------------------
// DMatrix wrapper
// ---------------------------------------------------------------------------

/// Safe wrapper around XGBoost DMatrix.
pub struct DMatrix {
    handle: DMatrixHandle,
}

impl DMatrix {
    /// Create a DMatrix from a row-major f32 matrix.
    pub fn from_dense(data: &[f32], nrow: usize, ncol: usize) -> Result<Self, String> {
        assert_eq!(data.len(), nrow * ncol, "data length mismatch");
        let mut handle: DMatrixHandle = ptr::null_mut();
        let ret = unsafe {
            XGDMatrixCreateFromMat(
                data.as_ptr(),
                nrow as BstUlong,
                ncol as BstUlong,
                f32::NAN,
                &mut handle,
            )
        };
        if ret != 0 {
            return Err(format!("XGDMatrixCreateFromMat failed (ret={})", ret));
        }
        Ok(Self { handle })
    }

    /// Set float info (e.g. "label") on the DMatrix.
    pub fn set_float_info(&self, field: &str, values: &[f32]) -> Result<(), String> {
        let c_field = CString::new(field).map_err(|e| e.to_string())?;
        let ret = unsafe {
            XGDMatrixSetFloatInfo(
                self.handle,
                c_field.as_ptr(),
                values.as_ptr(),
                values.len() as BstUlong,
            )
        };
        if ret != 0 {
            return Err(format!("XGDMatrixSetFloatInfo('{}') failed (ret={})", field, ret));
        }
        Ok(())
    }

    /// Set labels on the DMatrix.
    pub fn set_labels(&self, labels: &[f32]) -> Result<(), String> {
        self.set_float_info("label", labels)
    }

    pub(crate) fn raw(&self) -> DMatrixHandle {
        self.handle
    }
}

impl Drop for DMatrix {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { XGDMatrixFree(self.handle) };
        }
    }
}

// SAFETY: DMatrix is a handle to C-allocated memory that doesn't use TLS.
unsafe impl Send for DMatrix {}

// ---------------------------------------------------------------------------
// Booster wrapper
// ---------------------------------------------------------------------------

/// Safe wrapper around XGBoost Booster for training.
pub struct Booster {
    handle: BoosterHandle,
}

impl Booster {
    /// Create a new Booster from training DMatrices.
    pub fn new(dmats: &[&DMatrix]) -> Result<Self, String> {
        let handles: Vec<DMatrixHandle> = dmats.iter().map(|d| d.raw()).collect();
        let mut handle: BoosterHandle = ptr::null_mut();
        let ret = unsafe {
            XGBoosterCreate(
                handles.as_ptr(),
                handles.len() as BstUlong,
                &mut handle,
            )
        };
        if ret != 0 {
            return Err(format!("XGBoosterCreate failed (ret={})", ret));
        }
        Ok(Self { handle })
    }

    /// Set a parameter on the Booster (string key/value).
    pub fn set_param(&self, name: &str, value: &str) -> Result<(), String> {
        let c_name = CString::new(name).map_err(|e| e.to_string())?;
        let c_value = CString::new(value).map_err(|e| e.to_string())?;
        let ret = unsafe { XGBoosterSetParam(self.handle, c_name.as_ptr(), c_value.as_ptr()) };
        if ret != 0 {
            return Err(format!("XGBoosterSetParam('{}', '{}') failed (ret={})", name, value, ret));
        }
        Ok(())
    }

    /// Run one boosting iteration.
    pub fn update(&self, dtrain: &DMatrix, iter: u32) -> Result<(), String> {
        let ret = unsafe { XGBoosterUpdateOneIter(self.handle, iter as c_int, dtrain.raw()) };
        if ret != 0 {
            return Err(format!("XGBoosterUpdateOneIter(iter={}) failed (ret={})", iter, ret));
        }
        Ok(())
    }

    /// Predict using up to `ntree_limit` trees (0 = use all trees).
    pub fn predict(&self, dmat: &DMatrix, ntree_limit: u32) -> Result<Vec<f32>, String> {
        let mut out_len: BstUlong = 0;
        let mut out_result: *const f32 = ptr::null();
        let ret = unsafe {
            XGBoosterPredict(
                self.handle,
                dmat.raw(),
                0, // option_mask: 0 = normal prediction
                ntree_limit as c_uint,
                &mut out_len,
                &mut out_result,
            )
        };
        if ret != 0 {
            return Err(format!("XGBoosterPredict failed (ret={})", ret));
        }
        let slice = unsafe { std::slice::from_raw_parts(out_result, out_len as usize) };
        Ok(slice.to_vec())
    }
}

impl Drop for Booster {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { XGBoosterFree(self.handle) };
        }
    }
}

// SAFETY: Booster is a handle to C-allocated memory. XGBoost's C API is
// thread-safe for separate Booster instances.
unsafe impl Send for Booster {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dmatrix_create() {
        let data = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let dmat = DMatrix::from_dense(&data, 3, 2).expect("DMatrix creation");
        dmat.set_labels(&[0.0, 1.0, 0.0]).expect("set labels");
    }

    #[test]
    fn test_booster_create_and_train() {
        let data = vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let dmat = DMatrix::from_dense(&data, 4, 2).expect("DMatrix");
        dmat.set_labels(&[0.0, 1.0, 0.0, 1.0]).expect("labels");

        let booster = Booster::new(&[&dmat]).expect("Booster");
        booster.set_param("objective", "binary:logistic").expect("set objective");
        booster.set_param("max_depth", "3").expect("set max_depth");
        booster.set_param("eta", "0.1").expect("set eta");
        booster.set_param("reg_lambda", "6.586").expect("set reg_lambda (exact float)");
        booster.set_param("reg_alpha", "0.0014").expect("set reg_alpha (exact float)");
        booster.set_param("verbosity", "0").expect("set verbosity");

        // Train 10 rounds
        for i in 0..10 {
            booster.update(&dmat, i).expect("update");
        }

        // Predict with all trees
        let preds = booster.predict(&dmat, 0).expect("predict all");
        assert_eq!(preds.len(), 4);

        // Predict with ntree_limit = 5
        let preds_limited = booster.predict(&dmat, 5).expect("predict limited");
        assert_eq!(preds_limited.len(), 4);
    }
}
