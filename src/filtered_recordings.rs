use std::{any::Any, rc::Rc, sync::RwLock};

use crate::MapRecordings;
use once_cell::sync::Lazy;
use slint::{FilterModel, Model, ModelRc, VecModel};

static FILTER: Lazy<RwLock<String>> = Lazy::new(Default::default);

type InnerModel = Rc<VecModel<MapRecordings>>;

fn filter_function(rec: &MapRecordings) -> bool {
    let filter = FILTER.read().unwrap();

    rec.chapter_name.to_lowercase().contains(filter.as_str())
        || rec.map_bin.to_lowercase().contains(filter.as_str())
}

pub fn create_model(
    m: InnerModel,
) -> FilterModel<InnerModel, impl Fn(&MapRecordings) -> bool + 'static> {
    FilterModel::new(m, filter_function)
}

pub fn set_filter(
    filter: &str,
    filter_model: &FilterModel<InnerModel, impl Fn(&MapRecordings) -> bool + 'static>,
) {
    *FILTER.write().unwrap() = filter.to_lowercase();
    filter_model.reset();
}

pub fn get_source_vec_model(model: &ModelRc<MapRecordings>) -> &VecModel<MapRecordings> {
    fn name_fn_helper<M, F>(_: F, any: &dyn Any) -> Option<&FilterModel<M, F>>
    where
        M: Model + 'static,
        F: Fn(&M::Data) -> bool + 'static,
    {
        any.downcast_ref::<FilterModel<M, F>>()
    }

    let filter_model = name_fn_helper::<InnerModel, _>(filter_function, model.as_any()).unwrap();
    filter_model
        .source_model()
        .as_any()
        .downcast_ref::<VecModel<MapRecordings>>()
        .unwrap()
}
