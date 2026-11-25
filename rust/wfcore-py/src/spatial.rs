use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use wfcore_lib::spatial::{self, V};

#[pyclass]
pub struct SpatialIndex {
    inner: spatial::SpatialIndex,
}

#[pymethods]
impl SpatialIndex {
    #[new]
    fn new(vertices: Vec<V>, triangles: Vec<[usize; 3]>) -> PyResult<Self> {
        let inner = spatial::SpatialIndex::new(vertices, triangles).map_err(PyValueError::new_err)?;
        Ok(Self { inner })
    }

    #[pyo3(signature=(origin,direction,maxdist=f64::INFINITY,min_distance=1e-5))]
    fn raycast(&self, origin: V, direction: V, maxdist: f64, min_distance: f64) -> PyResult<Option<(f64, usize)>> {
        self.inner.raycast(origin, direction, maxdist, min_distance).map_err(PyValueError::new_err)
    }

    #[pyo3(signature=(origin,direction,maxdist=f64::INFINITY,min_distance=1e-5,max_hits=4096))]
    fn all_hits(&self, origin: V, direction: V, maxdist: f64, min_distance: f64, max_hits: usize) -> PyResult<Vec<(f64, usize)>> {
        self.inner.all_hits(origin, direction, maxdist, min_distance, max_hits).map_err(PyValueError::new_err)
    }

    fn raycast_batch(&self, rays: Vec<(V, V, f64)>) -> PyResult<Vec<Option<(f64, usize)>>> {
        self.inner.raycast_batch(rays).map_err(PyValueError::new_err)
    }
}

#[pyclass]
pub struct PathGraph {
    inner: spatial::PathGraph,
}

#[pymethods]
impl PathGraph {
    #[new]
    #[pyo3(signature = (node_count, edges))]
    fn new(node_count: usize, edges: Vec<(usize, usize, f64, usize)>) -> PyResult<Self> {
        let inner = spatial::PathGraph::new(node_count, edges).map_err(PyValueError::new_err)?;
        Ok(Self { inner })
    }

    #[pyo3(signature = (start, reverse, disabled_ids))]
    fn search(&self, py: Python<'_>, start: usize, reverse: bool, disabled_ids: Vec<usize>) -> PyResult<(Vec<f64>, Vec<i64>, Vec<i64>)> {
        py.detach(|| self.inner.search(start, reverse, disabled_ids)).map_err(PyValueError::new_err)
    }
}
