mod immediate;
mod multithreaded;
#[cfg(all(
    not(any(target_arch = "asmjs", target_arch = "wasm32")),
    feature = "rayon"
))]
mod rayon;

use crate::decoder::{choose_color_convert_func, ColorTransform};
use crate::error::Result;
use crate::parser::{Component, Dimensions};
use crate::upsampler::Upsampler;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;

pub struct RowData {
    pub index: usize,
    pub component: Component,
    pub quantization_table: Arc<[u16; 64]>,
}

pub trait Worker {
    fn start(&mut self, row_data: RowData) -> Result<()>;
    fn append_row(&mut self, row: (usize, Vec<i16>)) -> Result<()>;
    fn get_result(&mut self, index: usize) -> Result<Vec<u8>>;
    /// Default implementation for spawning multiple tasks.
    fn append_rows(&mut self, row: &mut dyn Iterator<Item = (usize, Vec<i16>)>) -> Result<()> {
        for item in row {
            self.append_row(item)?;
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub enum PreferWorkerKind {
    Immediate,
    Multithreaded,
}

#[derive(Default)]
pub struct WorkerScope {
    inner: core::cell::RefCell<Option<WorkerScopeInner>>,
}

enum WorkerScopeInner {
    #[cfg(all(
        not(any(target_arch = "asmjs", target_arch = "wasm32")),
        feature = "rayon"
    ))]
    Rayon(Box<rayon::Scoped>),
    #[cfg(not(any(target_arch = "asmjs", target_arch = "wasm32")))]
    Multithreaded(multithreaded::MpscWorker),
    Immediate(immediate::ImmediateWorker),
}

impl WorkerScope {
    pub fn with<T>(with: impl FnOnce(&Self) -> T) -> T {
        with(&WorkerScope {
            inner: RefCell::default(),
        })
    }

    pub fn get_or_init_worker<T>(
        &self,
        prefer: PreferWorkerKind,
        f: impl FnOnce(&mut dyn Worker) -> T,
    ) -> T {
        let mut inner = self.inner.borrow_mut();
        let inner = inner.get_or_insert_with(move || match prefer {
            #[cfg(all(
                not(any(target_arch = "asmjs", target_arch = "wasm32")),
                feature = "rayon"
            ))]
            PreferWorkerKind::Multithreaded => WorkerScopeInner::Rayon(Default::default()),
            #[allow(unreachable_patterns)]
            #[cfg(not(any(target_arch = "asmjs", target_arch = "wasm32")))]
            PreferWorkerKind::Multithreaded => WorkerScopeInner::Multithreaded(Default::default()),
            _ => WorkerScopeInner::Immediate(Default::default()),
        });

        f(match &mut *inner {
            #[cfg(all(
                not(any(target_arch = "asmjs", target_arch = "wasm32")),
                feature = "rayon"
            ))]
            WorkerScopeInner::Rayon(worker) => worker.as_mut(),
            #[cfg(not(any(target_arch = "asmjs", target_arch = "wasm32")))]
            WorkerScopeInner::Multithreaded(worker) => worker,
            WorkerScopeInner::Immediate(worker) => worker,
        })
    }
}

pub fn compute_image_parallel(
    components: &[Component],
    data: Vec<Vec<u8>>,
    output_size: Dimensions,
    color_transform: ColorTransform,
) -> Result<Vec<u8>> {
    #[cfg(all(
        not(any(target_arch = "asmjs", target_arch = "wasm32")),
        feature = "rayon"
    ))]
    return rayon::compute_image_parallel(components, data, output_size, color_transform);

    #[allow(unreachable_code)]
    {
        let color_convert_func = choose_color_convert_func(components.len(), color_transform)?;
        let upsampler = Upsampler::new(components, output_size.width, output_size.height)?;
        let line_size = output_size.width as usize * components.len();
        let mut image = vec![0u8; line_size * output_size.height as usize];

        for (row, line) in image.chunks_mut(line_size).enumerate() {
            upsampler.upsample_and_interleave_row(
                &data,
                row,
                output_size.width as usize,
                line,
                color_convert_func,
            );
        }

        Ok(image)
    }
}
