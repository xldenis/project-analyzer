#![feature(rustc_private)]
#![feature(impl_trait_in_assoc_type)]

extern crate rustc_abi;
extern crate rustc_codegen_ssa;
extern crate rustc_const_eval;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_monomorphize;
extern crate rustc_session;
extern crate rustc_span;

fn main() {
    let handler = EarlyDiagCtxt::new(ErrorOutputType::default());

    // Rust verification tools crash too much for the ice hook to report `full` by default
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "0");
    }

    rustc_driver::init_rustc_env_logger(&handler);
    let args = env::args().collect::<Vec<_>>();

    run_compiler(&args, &mut ProjectAnalyzer)
}

use std::{collections::HashMap, env};

use rustc_codegen_ssa::errors;
use rustc_const_eval::interpret::{AllocId, GlobalAlloc, Scalar};
use rustc_data_structures::fx::FxIndexMap;
use rustc_driver::{run_compiler, Callbacks, Compilation};
use rustc_hir::{self as hir, LangItem};
use rustc_hir::{
    def_id::{DefId, LocalDefId},
    intravisit::Visitor,
};
use rustc_middle::{
    middle::codegen_fn_attrs::CodegenFnAttrFlags,
    mir::{
        self,
        mono::{CollectionMode, MonoItem},
    },
    ty::{
        self, layout::ValidityRequirement, GenericArgs, GenericParamDefKind, Instance, Ty, TyCtxt,
    },
};
use rustc_session::{
    config::{EntryFnType, ErrorOutputType},
    EarlyDiagCtxt,
};

struct ProjectAnalyzer;

impl Callbacks for ProjectAnalyzer {
    fn after_expansion<'tcx>(
        &mut self,
        _compiler: &rustc_interface::interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> rustc_driver::Compilation {
        analyze_deps(tcx);

        Compilation::Continue
    }

    // fn after_analysis<'tcx>(
    //         &mut self,
    //         _compiler: &rustc_interface::interface::Compiler,
    //         tcx: TyCtxt<'tcx>,
    //     ) -> Compilation {
    //     let roots = collect_roots(tcx, MonoItemCollectionStrategy::Eager);

    //     let mut user_map : HashMap<DefId, HashMap<MonoItem<'tcx>, usize>> = Default::default();
    //     for r in roots {
    //         match r {
    //             MonoItem::Fn(instance) => {
    //                 let x = tcx.items_of_instance((instance, CollectionMode::UsedItems));
    //                 for item in x.0.into_iter().chain(x.1) {

    //                     *user_map.entry(instance.def_id()).or_default()
    //                         .entry(item.node).or_default() += 1;

    //                     // .entry(item.node.def_id()).or_default() += 1;
    //                 }
    //             },
    //            _ => (),
    //         }
    //     }

    //    let mut sorted : Vec<_> = user_map.into_iter().collect();
    //    sorted.sort_by_key(|k| k.1.iter().map(|(m, c)| c * m.size_estimate(tcx)).sum::<usize>());
    //    sorted.reverse();
    //     // sorted.sort_by_key(|k| k.1.values().sum::<usize>() );

    //     for (k, v) in &sorted[0..1] {
    //         if !k.is_local() {
    //             continue
    //         }

    //         eprintln!("{}", tcx.def_path_str(k));

    //         let mut items : Vec<_> = v.clone().into_iter().collect();
    //         items.sort_by_key(|(m, c)| c * m.size_estimate(tcx));
    //         items.reverse();
    //         for (v, _) in items {
    //             if let MonoItem::Fn(instance) = v {
    //                 if !instance.def_id().is_local() {
    //                     continue;
    //                 }
    //                 eprintln!("  {} {}", tcx.def_path_str_with_args(instance.def_id(), instance.args), tcx.size_estimate(instance))
    //             }
    //         }
    //     }
    //     Compilation::Continue
    // }
}

fn analyze_deps(tcx: TyCtxt) {
    let hir = tcx.hir();

    hir.visit_all_item_likes_in_crate(&mut PrintItem(tcx));
}

struct PrintItem<'tcx>(TyCtxt<'tcx>);
use rustc_hir::def::DefKind;
use rustc_span::{
    source_map::{dummy_spanned, respan, Spanned},
    Span, DUMMY_SP,
};

impl<'tcx> Visitor<'tcx> for PrintItem<'tcx> {
    fn visit_use(
        &mut self,
        path: &'tcx rustc_hir::UsePath<'tcx>,
        hir_id: rustc_hir::HirId,
    ) -> Self::Result {
        let local_res: Vec<_> = path
            .res
            .iter()
            .filter_map(|res| {
                let is_local = res.opt_def_id().map(|d| d.is_local()).unwrap_or(false);

                if is_local {
                    let is_mod = self.0.def_kind(res.def_id()) == DefKind::Mod;
                    if is_mod {
                        Some(res.def_id())
                    } else {
                        Some(
                            self.0
                                .parent_module_from_def_id(res.def_id().expect_local())
                                .to_def_id(),
                        )
                    }
                } else {
                    None
                }
            })
            .collect();

        let parent = self.0.parent_module(hir_id);
        for res in local_res {
            eprintln!(
                "{:?} -> {:?}",
                self.0.def_path_str(parent.to_def_id()),
                self.0.def_path_str(res)
            );
        }
    }
}

fn collect_roots(tcx: TyCtxt<'_>, mode: MonoItemCollectionStrategy) -> Vec<MonoItem<'_>> {
    let mut roots = MonoItems::new();

    {
        let entry_fn = tcx.entry_fn(());

        let mut collector = RootCollector {
            tcx,
            strategy: mode,
            entry_fn,
            output: &mut roots,
        };

        let crate_items = tcx.hir_crate_items(());

        for id in crate_items.free_items() {
            collector.process_item(id);
        }

        for id in crate_items.impl_items() {
            collector.process_impl_item(id);
        }

        collector.push_extra_entry_roots();
    }

    // We can only codegen items that are instantiable - items all of
    // whose predicates hold. Luckily, items that aren't instantiable
    // can't actually be used, so we can just skip codegenning them.
    roots
        .into_iter()
        .filter_map(
            |Spanned {
                 node: mono_item, ..
             }| { mono_item.is_instantiable(tcx).then_some(mono_item) },
        )
        .collect()
}

struct RootCollector<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    strategy: MonoItemCollectionStrategy,
    output: &'a mut MonoItems<'tcx>,
    entry_fn: Option<(DefId, EntryFnType)>,
}

impl<'v> RootCollector<'_, 'v> {
    fn process_item(&mut self, id: hir::ItemId) {
        match self.tcx.def_kind(id.owner_id) {
            DefKind::Enum | DefKind::Struct | DefKind::Union => {
                if self.strategy == MonoItemCollectionStrategy::Eager
                    && self.tcx.generics_of(id.owner_id).is_empty()
                {
                    // This type is impossible to instantiate, so we should not try to
                    // generate a `drop_in_place` instance for it.
                    if self.tcx.instantiate_and_check_impossible_predicates((
                        id.owner_id.to_def_id(),
                        ty::List::empty(),
                    )) {
                        return;
                    }

                    let ty = self
                        .tcx
                        .type_of(id.owner_id.to_def_id())
                        .no_bound_vars()
                        .unwrap();
                    visit_drop_use(self.tcx, ty, true, DUMMY_SP, self.output);
                }
            }
            DefKind::GlobalAsm => {
                self.output.push(dummy_spanned(MonoItem::GlobalAsm(id)));
            }
            DefKind::Static { .. } => {
                let def_id = id.owner_id.to_def_id();
                self.output.push(dummy_spanned(MonoItem::Static(def_id)));
            }
            DefKind::Const => {
                // const items only generate mono items if they are
                // actually used somewhere. Just declaring them is insufficient.

                // but even just declaring them must collect the items they refer to
                if let Ok(val) = self.tcx.const_eval_poly(id.owner_id.to_def_id()) {
                    collect_const_value(self.tcx, val, self.output);
                }
            }
            DefKind::Impl { .. } => {
                if self.strategy == MonoItemCollectionStrategy::Eager {
                    create_mono_items_for_default_impls(self.tcx, id, self.output);
                }
            }
            DefKind::Fn => {
                self.push_if_root(id.owner_id.def_id);
            }
            _ => {}
        }
    }

    fn process_impl_item(&mut self, id: hir::ImplItemId) {
        if matches!(self.tcx.def_kind(id.owner_id), DefKind::AssocFn) {
            self.push_if_root(id.owner_id.def_id);
        }
    }

    fn is_root(&self, def_id: LocalDefId) -> bool {
        !self
            .tcx
            .generics_of(def_id)
            .requires_monomorphization(self.tcx)
            && match self.strategy {
                MonoItemCollectionStrategy::Eager => true,
                MonoItemCollectionStrategy::Lazy => {
                    self.entry_fn.and_then(|(id, _)| id.as_local()) == Some(def_id)
                        || self.tcx.is_reachable_non_generic(def_id)
                        || self
                            .tcx
                            .codegen_fn_attrs(def_id)
                            .flags
                            .contains(CodegenFnAttrFlags::RUSTC_STD_INTERNAL_SYMBOL)
                }
            }
    }

    /// If `def_id` represents a root, pushes it onto the list of
    /// outputs. (Note that all roots must be monomorphic.)
    fn push_if_root(&mut self, def_id: LocalDefId) {
        if self.is_root(def_id) {
            let instance = Instance::mono(self.tcx, def_id.to_def_id());
            self.output
                .push(create_fn_mono_item(self.tcx, instance, DUMMY_SP));
        }
    }

    /// As a special case, when/if we encounter the
    /// `main()` function, we also have to generate a
    /// monomorphized copy of the start lang item based on
    /// the return type of `main`. This is not needed when
    /// the user writes their own `start` manually.
    fn push_extra_entry_roots(&mut self) {
        let Some((main_def_id, EntryFnType::Main { .. })) = self.entry_fn else {
            return;
        };

        let Some(start_def_id) = self.tcx.lang_items().start_fn() else {
            todo!()
        };
        let main_ret_ty = self
            .tcx
            .fn_sig(main_def_id)
            .no_bound_vars()
            .unwrap()
            .output();

        // Given that `main()` has no arguments,
        // then its return type cannot have
        // late-bound regions, since late-bound
        // regions must appear in the argument
        // listing.
        let main_ret_ty = self.tcx.normalize_erasing_regions(
            ty::TypingEnv::fully_monomorphized(),
            main_ret_ty.no_bound_vars().unwrap(),
        );

        let start_instance = Instance::expect_resolve(
            self.tcx,
            ty::TypingEnv::fully_monomorphized(),
            start_def_id,
            self.tcx.mk_args(&[main_ret_ty.into()]),
            DUMMY_SP,
        );

        self.output
            .push(create_fn_mono_item(self.tcx, start_instance, DUMMY_SP));
    }
}

fn create_fn_mono_item<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: Instance<'tcx>,
    source: Span,
) -> Spanned<MonoItem<'tcx>> {
    respan(source, MonoItem::Fn(instance))
}

fn create_mono_items_for_default_impls<'tcx>(
    tcx: TyCtxt<'tcx>,
    item: hir::ItemId,
    output: &mut MonoItems<'tcx>,
) {
    let Some(impl_) = tcx.impl_trait_header(item.owner_id) else {
        return;
    };

    if matches!(impl_.polarity, ty::ImplPolarity::Negative) {
        return;
    }

    if tcx
        .generics_of(item.owner_id)
        .own_requires_monomorphization()
    {
        return;
    }

    // Lifetimes never affect trait selection, so we are allowed to eagerly
    // instantiate an instance of an impl method if the impl (and method,
    // which we check below) is only parameterized over lifetime. In that case,
    // we use the ReErased, which has no lifetime information associated with
    // it, to validate whether or not the impl is legal to instantiate at all.
    let only_region_params = |param: &ty::GenericParamDef, _: &_| match param.kind {
        GenericParamDefKind::Lifetime => tcx.lifetimes.re_erased.into(),
        GenericParamDefKind::Type { .. } | GenericParamDefKind::Const { .. } => {
            unreachable!(
                "`own_requires_monomorphization` check means that \
                we should have no type/const params"
            )
        }
    };
    let impl_args = GenericArgs::for_item(tcx, item.owner_id.to_def_id(), only_region_params);
    let trait_ref = impl_.trait_ref.instantiate(tcx, impl_args);

    // Unlike 'lazy' monomorphization that begins by collecting items transitively
    // called by `main` or other global items, when eagerly monomorphizing impl
    // items, we never actually check that the predicates of this impl are satisfied
    // in a empty param env (i.e. with no assumptions).
    //
    // Even though this impl has no type or const generic parameters, because we don't
    // consider higher-ranked predicates such as `for<'a> &'a mut [u8]: Copy` to
    // be trivially false. We must now check that the impl has no impossible-to-satisfy
    // predicates.
    if tcx.instantiate_and_check_impossible_predicates((item.owner_id.to_def_id(), impl_args)) {
        return;
    }

    let typing_env = ty::TypingEnv::fully_monomorphized();
    let trait_ref = tcx.normalize_erasing_regions(typing_env, trait_ref);
    let overridden_methods = tcx.impl_item_implementor_ids(item.owner_id);
    for method in tcx.provided_trait_methods(trait_ref.def_id) {
        if overridden_methods.contains_key(&method.def_id) {
            continue;
        }

        if tcx
            .generics_of(method.def_id)
            .own_requires_monomorphization()
        {
            continue;
        }

        // As mentioned above, the method is legal to eagerly instantiate if it
        // only has lifetime generic parameters. This is validated by calling
        // `own_requires_monomorphization` on both the impl and method.
        let args = trait_ref
            .args
            .extend_to(tcx, method.def_id, only_region_params);
        let instance = ty::Instance::expect_resolve(tcx, typing_env, method.def_id, args, DUMMY_SP);

        let mono_item = create_fn_mono_item(tcx, instance, DUMMY_SP);
        if mono_item.node.is_instantiable(tcx) && tcx.should_codegen_locally(instance) {
            output.push(mono_item);
        }
    }
}

struct MonoItems<'tcx> {
    // We want a set of MonoItem + Span where trying to re-insert a MonoItem with a different Span
    // is ignored. Map does that, but it looks odd.
    items: FxIndexMap<MonoItem<'tcx>, Span>,
}

impl<'tcx> MonoItems<'tcx> {
    fn new() -> Self {
        Self {
            items: FxIndexMap::default(),
        }
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn push(&mut self, item: Spanned<MonoItem<'tcx>>) {
        // Insert only if the entry does not exist. A normal insert would stomp the first span that
        // got inserted.
        self.items.entry(item.node).or_insert(item.span);
    }

    fn items(&self) -> impl Iterator<Item = MonoItem<'tcx>> + '_ {
        self.items.keys().cloned()
    }
}

impl<'tcx> IntoIterator for MonoItems<'tcx> {
    type Item = Spanned<MonoItem<'tcx>>;
    type IntoIter = impl Iterator<Item = Spanned<MonoItem<'tcx>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.items
            .into_iter()
            .map(|(item, span)| respan(span, item))
    }
}

impl<'tcx> Extend<Spanned<MonoItem<'tcx>>> for MonoItems<'tcx> {
    fn extend<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = Spanned<MonoItem<'tcx>>>,
    {
        for item in iter {
            self.push(item)
        }
    }
}
#[derive(PartialEq)]
pub(crate) enum MonoItemCollectionStrategy {
    Eager,
    Lazy,
}

fn collect_const_value<'tcx>(
    tcx: TyCtxt<'tcx>,
    value: mir::ConstValue<'tcx>,
    output: &mut MonoItems<'tcx>,
) {
    match value {
        mir::ConstValue::Scalar(Scalar::Ptr(ptr, _size)) => {
            collect_alloc(tcx, ptr.provenance.alloc_id(), output)
        }
        mir::ConstValue::Indirect { alloc_id, .. } => collect_alloc(tcx, alloc_id, output),
        mir::ConstValue::Slice { data, meta: _ } => {
            for &prov in data.inner().provenance().ptrs().values() {
                collect_alloc(tcx, prov.alloc_id(), output);
            }
        }
        _ => {}
    }
}

/// Scans the CTFE alloc in order to find function pointers and statics that must be monomorphized.
fn collect_alloc<'tcx>(tcx: TyCtxt<'tcx>, alloc_id: AllocId, output: &mut MonoItems<'tcx>) {
    match tcx.global_alloc(alloc_id) {
        GlobalAlloc::Static(def_id) => {
            assert!(!tcx.is_thread_local_static(def_id));
            let instance = Instance::mono(tcx, def_id);
            if tcx.should_codegen_locally(instance) {
                output.push(dummy_spanned(MonoItem::Static(def_id)));
            }
        }
        GlobalAlloc::Memory(alloc) => {
            let ptrs = alloc.inner().provenance().ptrs();
            // avoid `ensure_sufficient_stack` in the common case of "no pointers"
            if !ptrs.is_empty() {
                rustc_data_structures::stack::ensure_sufficient_stack(move || {
                    for &prov in ptrs.values() {
                        collect_alloc(tcx, prov.alloc_id(), output);
                    }
                });
            }
        }
        GlobalAlloc::Function { instance, .. } => {
            if tcx.should_codegen_locally(instance) {
                output.push(create_fn_mono_item(tcx, instance, DUMMY_SP));
            }
        }
        GlobalAlloc::VTable(ty, dyn_ty) => {
            let alloc_id = tcx.vtable_allocation((
                ty,
                dyn_ty
                    .principal()
                    .map(|principal| tcx.instantiate_bound_regions_with_erased(principal)),
            ));
            collect_alloc(tcx, alloc_id, output)
        }
    }
}

fn visit_drop_use<'tcx>(
    tcx: TyCtxt<'tcx>,
    ty: Ty<'tcx>,
    is_direct_call: bool,
    source: Span,
    output: &mut MonoItems<'tcx>,
) {
    let instance = Instance::resolve_drop_in_place(tcx, ty);
    visit_instance_use(tcx, instance, is_direct_call, source, output);
}

fn visit_instance_use<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: ty::Instance<'tcx>,
    is_direct_call: bool,
    source: Span,
    output: &mut MonoItems<'tcx>,
) {
    if !tcx.should_codegen_locally(instance) {
        return;
    }
    if let Some(intrinsic) = tcx.intrinsic(instance.def_id()) {
        if let Some(_requirement) = ValidityRequirement::from_intrinsic(intrinsic.name) {
            // The intrinsics assert_inhabited, assert_zero_valid, and assert_mem_uninitialized_valid will
            // be lowered in codegen to nothing or a call to panic_nounwind. So if we encounter any
            // of those intrinsics, we need to include a mono item for panic_nounwind, else we may try to
            // codegen a call to that function without generating code for the function itself.
            let def_id = tcx.require_lang_item(LangItem::PanicNounwind, None);
            let panic_instance = Instance::mono(tcx, def_id);
            if tcx.should_codegen_locally(panic_instance) {
                output.push(create_fn_mono_item(tcx, panic_instance, source));
            }
        } else if !intrinsic.must_be_overridden {
            // Codegen the fallback body of intrinsics with fallback bodies.
            // We explicitly skip this otherwise to ensure we get a linker error
            // if anyone tries to call this intrinsic and the codegen backend did not
            // override the implementation.
            let instance = ty::Instance::new(instance.def_id(), instance.args);
            if tcx.should_codegen_locally(instance) {
                output.push(create_fn_mono_item(tcx, instance, source));
            }
        }
    }

    match instance.def {
        ty::InstanceKind::Virtual(..) | ty::InstanceKind::Intrinsic(_) => {
            if !is_direct_call {
                todo!("{:?} being reified", instance);
            }
        }
        ty::InstanceKind::ThreadLocalShim(..) => {
            todo!("{:?} being reified", instance);
        }
        ty::InstanceKind::DropGlue(_, None) | ty::InstanceKind::AsyncDropGlueCtorShim(_, None) => {
            // Don't need to emit noop drop glue if we are calling directly.
            if !is_direct_call {
                output.push(create_fn_mono_item(tcx, instance, source));
            }
        }
        ty::InstanceKind::DropGlue(_, Some(_))
        | ty::InstanceKind::AsyncDropGlueCtorShim(_, Some(_))
        | ty::InstanceKind::VTableShim(..)
        | ty::InstanceKind::ReifyShim(..)
        | ty::InstanceKind::ClosureOnceShim { .. }
        | ty::InstanceKind::ConstructCoroutineInClosureShim { .. }
        | ty::InstanceKind::Item(..)
        | ty::InstanceKind::FnPtrShim(..)
        | ty::InstanceKind::CloneShim(..)
        | ty::InstanceKind::FnPtrAddrShim(..) => {
            output.push(create_fn_mono_item(tcx, instance, source));
        }
    }
}
