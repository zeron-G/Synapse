use pyo3::prelude::*;
use pyo3::types::PyBytes;
use synapse_core::{self, error::SynapseError};

/// A Synapse bridge endpoint exposed to Python.
#[pyclass]
struct Bridge {
    inner: synapse_core::Bridge,
}

#[pymethods]
impl Bridge {
    /// Send bytes through the bridge. GIL is released during the shm operation.
    fn send(&self, py: Python<'_>, data: &[u8]) -> PyResult<()> {
        let inner = &self.inner;
        py.allow_threads(|| {
            inner.send(data)
        })
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Receive bytes from the bridge. Returns None if no data available.
    /// GIL is released during the shm operation.
    fn recv<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let inner = &self.inner;
        let result = py.allow_threads(|| inner.recv());
        match result {
            Some(data) => Ok(Some(PyBytes::new(py, &data))),
            None => Ok(None),
        }
    }

    /// Check if the bridge is ready.
    fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }

    /// Get the session token as a hex string.
    fn session_token(&self) -> String {
        format!("{:032x}", self.inner.session_token())
    }
}

/// Create a Synapse bridge as the host (creator).
///
/// Args:
///     name: Name of the shared memory region.
///     capacity: Number of slots (must be power of 2). Default: 1024.
///     slot_size: Size of each slot in bytes. Default: 256.
#[pyfunction]
#[pyo3(signature = (name, capacity=1024, slot_size=256))]
fn host(name: &str, capacity: u64, slot_size: u64) -> PyResult<Bridge> {
    let inner = synapse_core::host_with_config(name, capacity, slot_size)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    Ok(Bridge { inner })
}

/// Connect to an existing Synapse bridge.
///
/// Args:
///     name: Name of the shared memory region.
///     capacity: Must match the host's capacity. Default: 1024.
///     slot_size: Must match the host's slot_size. Default: 256.
#[pyfunction]
#[pyo3(signature = (name, capacity=1024, slot_size=256))]
fn connect(name: &str, capacity: u64, slot_size: u64) -> PyResult<Bridge> {
    let inner = synapse_core::connect_with_config(name, capacity, slot_size)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    Ok(Bridge { inner })
}

/// Synapse — Cross-language runtime bridge.
#[pymodule]
fn synapse(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(host, m)?)?;
    m.add_function(wrap_pyfunction!(connect, m)?)?;
    m.add_class::<Bridge>()?;
    Ok(())
}
