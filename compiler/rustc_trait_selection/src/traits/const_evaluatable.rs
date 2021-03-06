//! Checking that constant values used in types can be successfully evaluated.
//!
//! For concrete constants, this is fairly simple as we can just try and evaluate it.
//!
//! When dealing with polymorphic constants, for example `std::mem::size_of::<T>() - 1`,
//! this is not as easy.
//!
//! In this case we try to build an abstract representation of this constant using
//! `mir_abstract_const` which can then be checked for structural equality with other
//! generic constants mentioned in the `caller_bounds` of the current environment.
use rustc_errors::ErrorReported;
use rustc_hir::def::DefKind;
use rustc_index::bit_set::BitSet;
use rustc_index::vec::IndexVec;
use rustc_infer::infer::InferCtxt;
use rustc_middle::mir::abstract_const::{Node, NodeId, NotConstEvaluatable};
use rustc_middle::mir::interpret::ErrorHandled;
use rustc_middle::mir::{self, Rvalue, StatementKind, TerminatorKind};
use rustc_middle::ty::subst::{Subst, SubstsRef};
use rustc_middle::ty::{self, TyCtxt, TypeFoldable};
use rustc_session::lint;
use rustc_span::def_id::LocalDefId;
use rustc_span::Span;

use std::cmp;
use std::iter;
use std::ops::ControlFlow;

/// Check if a given constant can be evaluated.
pub fn is_const_evaluatable<'cx, 'tcx>(
    infcx: &InferCtxt<'cx, 'tcx>,
    uv: ty::Unevaluated<'tcx, ()>,
    param_env: ty::ParamEnv<'tcx>,
    span: Span,
) -> Result<(), NotConstEvaluatable> {
    debug!("is_const_evaluatable({:?})", uv);
    if infcx.tcx.features().generic_const_exprs {
        let tcx = infcx.tcx;
        match AbstractConst::new(tcx, uv)? {
            // We are looking at a generic abstract constant.
            Some(ct) => {
                for pred in param_env.caller_bounds() {
                    match pred.kind().skip_binder() {
                        ty::PredicateKind::ConstEvaluatable(uv) => {
                            if let Some(b_ct) = AbstractConst::new(tcx, uv)? {
                                // Try to unify with each subtree in the AbstractConst to allow for
                                // `N + 1` being const evaluatable even if theres only a `ConstEvaluatable`
                                // predicate for `(N + 1) * 2`
                                let result =
                                    walk_abstract_const(tcx, b_ct, |b_ct| {
                                        match try_unify(tcx, ct, b_ct) {
                                            true => ControlFlow::BREAK,
                                            false => ControlFlow::CONTINUE,
                                        }
                                    });

                                if let ControlFlow::Break(()) = result {
                                    debug!("is_const_evaluatable: abstract_const ~~> ok");
                                    return Ok(());
                                }
                            }
                        }
                        _ => {} // don't care
                    }
                }

                // We were unable to unify the abstract constant with
                // a constant found in the caller bounds, there are
                // now three possible cases here.
                #[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
                enum FailureKind {
                    /// The abstract const still references an inference
                    /// variable, in this case we return `TooGeneric`.
                    MentionsInfer,
                    /// The abstract const references a generic parameter,
                    /// this means that we emit an error here.
                    MentionsParam,
                    /// The substs are concrete enough that we can simply
                    /// try and evaluate the given constant.
                    Concrete,
                }
                let mut failure_kind = FailureKind::Concrete;
                walk_abstract_const::<!, _>(tcx, ct, |node| match node.root() {
                    Node::Leaf(leaf) => {
                        let leaf = leaf.subst(tcx, ct.substs);
                        if leaf.has_infer_types_or_consts() {
                            failure_kind = FailureKind::MentionsInfer;
                        } else if leaf.definitely_has_param_types_or_consts(tcx) {
                            failure_kind = cmp::min(failure_kind, FailureKind::MentionsParam);
                        }

                        ControlFlow::CONTINUE
                    }
                    Node::Cast(_, _, ty) => {
                        let ty = ty.subst(tcx, ct.substs);
                        if ty.has_infer_types_or_consts() {
                            failure_kind = FailureKind::MentionsInfer;
                        } else if ty.definitely_has_param_types_or_consts(tcx) {
                            failure_kind = cmp::min(failure_kind, FailureKind::MentionsParam);
                        }

                        ControlFlow::CONTINUE
                    }
                    Node::Binop(_, _, _) | Node::UnaryOp(_, _) | Node::FunctionCall(_, _) => {
                        ControlFlow::CONTINUE
                    }
                });

                match failure_kind {
                    FailureKind::MentionsInfer => {
                        return Err(NotConstEvaluatable::MentionsInfer);
                    }
                    FailureKind::MentionsParam => {
                        return Err(NotConstEvaluatable::MentionsParam);
                    }
                    FailureKind::Concrete => {
                        // Dealt with below by the same code which handles this
                        // without the feature gate.
                    }
                }
            }
            None => {
                // If we are dealing with a concrete constant, we can
                // reuse the old code path and try to evaluate
                // the constant.
            }
        }
    }

    let future_compat_lint = || {
        if let Some(local_def_id) = uv.def.did.as_local() {
            infcx.tcx.struct_span_lint_hir(
                lint::builtin::CONST_EVALUATABLE_UNCHECKED,
                infcx.tcx.hir().local_def_id_to_hir_id(local_def_id),
                span,
                |err| {
                    err.build("cannot use constants which depend on generic parameters in types")
                        .emit();
                },
            );
        }
    };

    // FIXME: We should only try to evaluate a given constant here if it is fully concrete
    // as we don't want to allow things like `[u8; std::mem::size_of::<*mut T>()]`.
    //
    // We previously did not check this, so we only emit a future compat warning if
    // const evaluation succeeds and the given constant is still polymorphic for now
    // and hopefully soon change this to an error.
    //
    // See #74595 for more details about this.
    let concrete = infcx.const_eval_resolve(param_env, uv.expand(), Some(span));

    if concrete.is_ok() && uv.substs(infcx.tcx).definitely_has_param_types_or_consts(infcx.tcx) {
        match infcx.tcx.def_kind(uv.def.did) {
            DefKind::AnonConst => {
                let mir_body = infcx.tcx.mir_for_ctfe_opt_const_arg(uv.def);

                if mir_body.is_polymorphic {
                    future_compat_lint();
                }
            }
            _ => future_compat_lint(),
        }
    }

    debug!(?concrete, "is_const_evaluatable");
    match concrete {
        Err(ErrorHandled::TooGeneric) => Err(match uv.has_infer_types_or_consts() {
            true => NotConstEvaluatable::MentionsInfer,
            false => NotConstEvaluatable::MentionsParam,
        }),
        Err(ErrorHandled::Linted) => {
            infcx.tcx.sess.delay_span_bug(span, "constant in type had error reported as lint");
            Err(NotConstEvaluatable::Error(ErrorReported))
        }
        Err(ErrorHandled::Reported(e)) => Err(NotConstEvaluatable::Error(e)),
        Ok(_) => Ok(()),
    }
}

/// A tree representing an anonymous constant.
///
/// This is only able to represent a subset of `MIR`,
/// and should not leak any information about desugarings.
#[derive(Debug, Clone, Copy)]
pub struct AbstractConst<'tcx> {
    // FIXME: Consider adding something like `IndexSlice`
    // and use this here.
    pub inner: &'tcx [Node<'tcx>],
    pub substs: SubstsRef<'tcx>,
}

impl<'tcx> AbstractConst<'tcx> {
    pub fn new(
        tcx: TyCtxt<'tcx>,
        uv: ty::Unevaluated<'tcx, ()>,
    ) -> Result<Option<AbstractConst<'tcx>>, ErrorReported> {
        let inner = tcx.mir_abstract_const_opt_const_arg(uv.def)?;
        debug!("AbstractConst::new({:?}) = {:?}", uv, inner);
        Ok(inner.map(|inner| AbstractConst { inner, substs: uv.substs(tcx) }))
    }

    pub fn from_const(
        tcx: TyCtxt<'tcx>,
        ct: &ty::Const<'tcx>,
    ) -> Result<Option<AbstractConst<'tcx>>, ErrorReported> {
        match ct.val {
            ty::ConstKind::Unevaluated(uv) => AbstractConst::new(tcx, uv.shrink()),
            ty::ConstKind::Error(_) => Err(ErrorReported),
            _ => Ok(None),
        }
    }

    #[inline]
    pub fn subtree(self, node: NodeId) -> AbstractConst<'tcx> {
        AbstractConst { inner: &self.inner[..=node.index()], substs: self.substs }
    }

    #[inline]
    pub fn root(self) -> Node<'tcx> {
        self.inner.last().copied().unwrap()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkNode<'tcx> {
    node: Node<'tcx>,
    span: Span,
    used: bool,
}

struct AbstractConstBuilder<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    body: &'a mir::Body<'tcx>,
    /// The current WIP node tree.
    ///
    /// We require all nodes to be used in the final abstract const,
    /// so we store this here. Note that we also consider nodes as used
    /// if they are mentioned in an assert, so some used nodes are never
    /// actually reachable by walking the [`AbstractConst`].
    nodes: IndexVec<NodeId, WorkNode<'tcx>>,
    locals: IndexVec<mir::Local, NodeId>,
    /// We only allow field accesses if they access
    /// the result of a checked operation.
    checked_op_locals: BitSet<mir::Local>,
}

impl<'a, 'tcx> AbstractConstBuilder<'a, 'tcx> {
    fn error(&mut self, span: Option<Span>, msg: &str) -> Result<!, ErrorReported> {
        self.tcx
            .sess
            .struct_span_err(self.body.span, "overly complex generic constant")
            .span_label(span.unwrap_or(self.body.span), msg)
            .help("consider moving this anonymous constant into a `const` function")
            .emit();

        Err(ErrorReported)
    }

    fn new(
        tcx: TyCtxt<'tcx>,
        body: &'a mir::Body<'tcx>,
    ) -> Result<Option<AbstractConstBuilder<'a, 'tcx>>, ErrorReported> {
        let mut builder = AbstractConstBuilder {
            tcx,
            body,
            nodes: IndexVec::new(),
            locals: IndexVec::from_elem(NodeId::MAX, &body.local_decls),
            checked_op_locals: BitSet::new_empty(body.local_decls.len()),
        };

        // We don't have to look at concrete constants, as we
        // can just evaluate them.
        if !body.is_polymorphic {
            return Ok(None);
        }

        // We only allow consts without control flow, so
        // we check for cycles here which simplifies the
        // rest of this implementation.
        if body.is_cfg_cyclic() {
            builder.error(None, "cyclic anonymous constants are forbidden")?;
        }

        Ok(Some(builder))
    }

    fn add_node(&mut self, node: Node<'tcx>, span: Span) -> NodeId {
        // Mark used nodes.
        match node {
            Node::Leaf(_) => (),
            Node::Binop(_, lhs, rhs) => {
                self.nodes[lhs].used = true;
                self.nodes[rhs].used = true;
            }
            Node::UnaryOp(_, input) => {
                self.nodes[input].used = true;
            }
            Node::FunctionCall(func, nodes) => {
                self.nodes[func].used = true;
                nodes.iter().for_each(|&n| self.nodes[n].used = true);
            }
            Node::Cast(_, operand, _) => {
                self.nodes[operand].used = true;
            }
        }

        // Nodes start as unused.
        self.nodes.push(WorkNode { node, span, used: false })
    }

    fn place_to_local(
        &mut self,
        span: Span,
        p: &mir::Place<'tcx>,
    ) -> Result<mir::Local, ErrorReported> {
        const ZERO_FIELD: mir::Field = mir::Field::from_usize(0);
        // Do not allow any projections.
        //
        // One exception are field accesses on the result of checked operations,
        // which are required to support things like `1 + 2`.
        if let Some(p) = p.as_local() {
            debug_assert!(!self.checked_op_locals.contains(p));
            Ok(p)
        } else if let &[mir::ProjectionElem::Field(ZERO_FIELD, _)] = p.projection.as_ref() {
            // Only allow field accesses if the given local
            // contains the result of a checked operation.
            if self.checked_op_locals.contains(p.local) {
                Ok(p.local)
            } else {
                self.error(Some(span), "unsupported projection")?;
            }
        } else {
            self.error(Some(span), "unsupported projection")?;
        }
    }

    fn operand_to_node(
        &mut self,
        span: Span,
        op: &mir::Operand<'tcx>,
    ) -> Result<NodeId, ErrorReported> {
        debug!("operand_to_node: op={:?}", op);
        match op {
            mir::Operand::Copy(p) | mir::Operand::Move(p) => {
                let local = self.place_to_local(span, p)?;
                Ok(self.locals[local])
            }
            mir::Operand::Constant(ct) => match ct.literal {
                mir::ConstantKind::Ty(ct) => Ok(self.add_node(Node::Leaf(ct), span)),
                mir::ConstantKind::Val(..) => self.error(Some(span), "unsupported constant")?,
            },
        }
    }

    /// We do not allow all binary operations in abstract consts, so filter disallowed ones.
    fn check_binop(op: mir::BinOp) -> bool {
        use mir::BinOp::*;
        match op {
            Add | Sub | Mul | Div | Rem | BitXor | BitAnd | BitOr | Shl | Shr | Eq | Lt | Le
            | Ne | Ge | Gt => true,
            Offset => false,
        }
    }

    /// While we currently allow all unary operations, we still want to explicitly guard against
    /// future changes here.
    fn check_unop(op: mir::UnOp) -> bool {
        use mir::UnOp::*;
        match op {
            Not | Neg => true,
        }
    }

    fn build_statement(&mut self, stmt: &mir::Statement<'tcx>) -> Result<(), ErrorReported> {
        debug!("AbstractConstBuilder: stmt={:?}", stmt);
        let span = stmt.source_info.span;
        match stmt.kind {
            StatementKind::Assign(box (ref place, ref rvalue)) => {
                let local = self.place_to_local(span, place)?;
                match *rvalue {
                    Rvalue::Use(ref operand) => {
                        self.locals[local] = self.operand_to_node(span, operand)?;
                        Ok(())
                    }
                    Rvalue::BinaryOp(op, box (ref lhs, ref rhs)) if Self::check_binop(op) => {
                        let lhs = self.operand_to_node(span, lhs)?;
                        let rhs = self.operand_to_node(span, rhs)?;
                        self.locals[local] = self.add_node(Node::Binop(op, lhs, rhs), span);
                        if op.is_checkable() {
                            bug!("unexpected unchecked checkable binary operation");
                        } else {
                            Ok(())
                        }
                    }
                    Rvalue::CheckedBinaryOp(op, box (ref lhs, ref rhs))
                        if Self::check_binop(op) =>
                    {
                        let lhs = self.operand_to_node(span, lhs)?;
                        let rhs = self.operand_to_node(span, rhs)?;
                        self.locals[local] = self.add_node(Node::Binop(op, lhs, rhs), span);
                        self.checked_op_locals.insert(local);
                        Ok(())
                    }
                    Rvalue::UnaryOp(op, ref operand) if Self::check_unop(op) => {
                        let operand = self.operand_to_node(span, operand)?;
                        self.locals[local] = self.add_node(Node::UnaryOp(op, operand), span);
                        Ok(())
                    }
                    Rvalue::Cast(cast_kind, ref operand, ty) => {
                        let operand = self.operand_to_node(span, operand)?;
                        self.locals[local] =
                            self.add_node(Node::Cast(cast_kind, operand, ty), span);
                        Ok(())
                    }
                    _ => self.error(Some(span), "unsupported rvalue")?,
                }
            }
            // These are not actually relevant for us here, so we can ignore them.
            StatementKind::AscribeUserType(..)
            | StatementKind::StorageLive(_)
            | StatementKind::StorageDead(_) => Ok(()),
            _ => self.error(Some(stmt.source_info.span), "unsupported statement")?,
        }
    }

    /// Possible return values:
    ///
    /// - `None`: unsupported terminator, stop building
    /// - `Some(None)`: supported terminator, finish building
    /// - `Some(Some(block))`: support terminator, build `block` next
    fn build_terminator(
        &mut self,
        terminator: &mir::Terminator<'tcx>,
    ) -> Result<Option<mir::BasicBlock>, ErrorReported> {
        debug!("AbstractConstBuilder: terminator={:?}", terminator);
        match terminator.kind {
            TerminatorKind::Goto { target } => Ok(Some(target)),
            TerminatorKind::Return => Ok(None),
            TerminatorKind::Call {
                ref func,
                ref args,
                destination: Some((ref place, target)),
                // We do not care about `cleanup` here. Any branch which
                // uses `cleanup` will fail const-eval and they therefore
                // do not matter when checking for const evaluatability.
                //
                // Do note that even if `panic::catch_unwind` is made const,
                // we still do not have to care about this, as we do not look
                // into functions.
                cleanup: _,
                // Do not allow overloaded operators for now,
                // we probably do want to allow this in the future.
                //
                // This is currently fairly irrelevant as it requires `const Trait`s.
                from_hir_call: true,
                fn_span,
            } => {
                let local = self.place_to_local(fn_span, place)?;
                let func = self.operand_to_node(fn_span, func)?;
                let args = self.tcx.arena.alloc_from_iter(
                    args.iter()
                        .map(|arg| self.operand_to_node(terminator.source_info.span, arg))
                        .collect::<Result<Vec<NodeId>, _>>()?,
                );
                self.locals[local] = self.add_node(Node::FunctionCall(func, args), fn_span);
                Ok(Some(target))
            }
            TerminatorKind::Assert { ref cond, expected: false, target, .. } => {
                let p = match cond {
                    mir::Operand::Copy(p) | mir::Operand::Move(p) => p,
                    mir::Operand::Constant(_) => bug!("unexpected assert"),
                };

                const ONE_FIELD: mir::Field = mir::Field::from_usize(1);
                debug!("proj: {:?}", p.projection);
                if let Some(p) = p.as_local() {
                    debug_assert!(!self.checked_op_locals.contains(p));
                    // Mark locals directly used in asserts as used.
                    //
                    // This is needed because division does not use `CheckedBinop` but instead
                    // adds an explicit assert for `divisor != 0`.
                    self.nodes[self.locals[p]].used = true;
                    return Ok(Some(target));
                } else if let &[mir::ProjectionElem::Field(ONE_FIELD, _)] = p.projection.as_ref() {
                    // Only allow asserts checking the result of a checked operation.
                    if self.checked_op_locals.contains(p.local) {
                        return Ok(Some(target));
                    }
                }

                self.error(Some(terminator.source_info.span), "unsupported assertion")?;
            }
            _ => self.error(Some(terminator.source_info.span), "unsupported terminator")?,
        }
    }

    /// Builds the abstract const by walking the mir from start to finish
    /// and bailing out when encountering an unsupported operation.
    fn build(mut self) -> Result<&'tcx [Node<'tcx>], ErrorReported> {
        let mut block = &self.body.basic_blocks()[mir::START_BLOCK];
        // We checked for a cyclic cfg above, so this should terminate.
        loop {
            debug!("AbstractConstBuilder: block={:?}", block);
            for stmt in block.statements.iter() {
                self.build_statement(stmt)?;
            }

            if let Some(next) = self.build_terminator(block.terminator())? {
                block = &self.body.basic_blocks()[next];
            } else {
                break;
            }
        }

        assert_eq!(self.locals[mir::RETURN_PLACE], self.nodes.last().unwrap());
        for n in self.nodes.iter() {
            if let Node::Leaf(ty::Const { val: ty::ConstKind::Unevaluated(ct), ty: _ }) = n.node {
                // `AbstractConst`s should not contain any promoteds as they require references which
                // are not allowed.
                assert_eq!(ct.promoted, None);
            }
        }

        self.nodes[self.locals[mir::RETURN_PLACE]].used = true;
        if let Some(&unused) = self.nodes.iter().find(|n| !n.used) {
            self.error(Some(unused.span), "dead code")?;
        }

        Ok(self.tcx.arena.alloc_from_iter(self.nodes.into_iter().map(|n| n.node)))
    }
}

/// Builds an abstract const, do not use this directly, but use `AbstractConst::new` instead.
pub(super) fn mir_abstract_const<'tcx>(
    tcx: TyCtxt<'tcx>,
    def: ty::WithOptConstParam<LocalDefId>,
) -> Result<Option<&'tcx [mir::abstract_const::Node<'tcx>]>, ErrorReported> {
    if tcx.features().generic_const_exprs {
        match tcx.def_kind(def.did) {
            // FIXME(generic_const_exprs): We currently only do this for anonymous constants,
            // meaning that we do not look into associated constants. I(@lcnr) am not yet sure whether
            // we want to look into them or treat them as opaque projections.
            //
            // Right now we do neither of that and simply always fail to unify them.
            DefKind::AnonConst => (),
            _ => return Ok(None),
        }
        let body = tcx.mir_const(def).borrow();
        AbstractConstBuilder::new(tcx, &body)?.map(AbstractConstBuilder::build).transpose()
    } else {
        Ok(None)
    }
}

pub(super) fn try_unify_abstract_consts<'tcx>(
    tcx: TyCtxt<'tcx>,
    (a, b): (ty::Unevaluated<'tcx, ()>, ty::Unevaluated<'tcx, ()>),
) -> bool {
    (|| {
        if let Some(a) = AbstractConst::new(tcx, a)? {
            if let Some(b) = AbstractConst::new(tcx, b)? {
                return Ok(try_unify(tcx, a, b));
            }
        }

        Ok(false)
    })()
    .unwrap_or_else(|ErrorReported| true)
    // FIXME(generic_const_exprs): We should instead have this
    // method return the resulting `ty::Const` and return `ConstKind::Error`
    // on `ErrorReported`.
}

pub fn walk_abstract_const<'tcx, R, F>(
    tcx: TyCtxt<'tcx>,
    ct: AbstractConst<'tcx>,
    mut f: F,
) -> ControlFlow<R>
where
    F: FnMut(AbstractConst<'tcx>) -> ControlFlow<R>,
{
    fn recurse<'tcx, R>(
        tcx: TyCtxt<'tcx>,
        ct: AbstractConst<'tcx>,
        f: &mut dyn FnMut(AbstractConst<'tcx>) -> ControlFlow<R>,
    ) -> ControlFlow<R> {
        f(ct)?;
        let root = ct.root();
        match root {
            Node::Leaf(_) => ControlFlow::CONTINUE,
            Node::Binop(_, l, r) => {
                recurse(tcx, ct.subtree(l), f)?;
                recurse(tcx, ct.subtree(r), f)
            }
            Node::UnaryOp(_, v) => recurse(tcx, ct.subtree(v), f),
            Node::FunctionCall(func, args) => {
                recurse(tcx, ct.subtree(func), f)?;
                args.iter().try_for_each(|&arg| recurse(tcx, ct.subtree(arg), f))
            }
            Node::Cast(_, operand, _) => recurse(tcx, ct.subtree(operand), f),
        }
    }

    recurse(tcx, ct, &mut f)
}

/// Tries to unify two abstract constants using structural equality.
pub(super) fn try_unify<'tcx>(
    tcx: TyCtxt<'tcx>,
    mut a: AbstractConst<'tcx>,
    mut b: AbstractConst<'tcx>,
) -> bool {
    // We substitute generics repeatedly to allow AbstractConsts to unify where a
    // ConstKind::Unevalated could be turned into an AbstractConst that would unify e.g.
    // Param(N) should unify with Param(T), substs: [Unevaluated("T2", [Unevaluated("T3", [Param(N)])])]
    while let Node::Leaf(a_ct) = a.root() {
        let a_ct = a_ct.subst(tcx, a.substs);
        match AbstractConst::from_const(tcx, a_ct) {
            Ok(Some(a_act)) => a = a_act,
            Ok(None) => break,
            Err(_) => return true,
        }
    }
    while let Node::Leaf(b_ct) = b.root() {
        let b_ct = b_ct.subst(tcx, b.substs);
        match AbstractConst::from_const(tcx, b_ct) {
            Ok(Some(b_act)) => b = b_act,
            Ok(None) => break,
            Err(_) => return true,
        }
    }

    match (a.root(), b.root()) {
        (Node::Leaf(a_ct), Node::Leaf(b_ct)) => {
            let a_ct = a_ct.subst(tcx, a.substs);
            let b_ct = b_ct.subst(tcx, b.substs);
            if a_ct.ty != b_ct.ty {
                return false;
            }

            match (a_ct.val, b_ct.val) {
                // We can just unify errors with everything to reduce the amount of
                // emitted errors here.
                (ty::ConstKind::Error(_), _) | (_, ty::ConstKind::Error(_)) => true,
                (ty::ConstKind::Param(a_param), ty::ConstKind::Param(b_param)) => {
                    a_param == b_param
                }
                (ty::ConstKind::Value(a_val), ty::ConstKind::Value(b_val)) => a_val == b_val,
                // If we have `fn a<const N: usize>() -> [u8; N + 1]` and `fn b<const M: usize>() -> [u8; 1 + M]`
                // we do not want to use `assert_eq!(a(), b())` to infer that `N` and `M` have to be `1`. This
                // means that we only allow inference variables if they are equal.
                (ty::ConstKind::Infer(a_val), ty::ConstKind::Infer(b_val)) => a_val == b_val,
                // We expand generic anonymous constants at the start of this function, so this
                // branch should only be taking when dealing with associated constants, at
                // which point directly comparing them seems like the desired behavior.
                //
                // FIXME(generic_const_exprs): This isn't actually the case.
                // We also take this branch for concrete anonymous constants and
                // expand generic anonymous constants with concrete substs.
                (ty::ConstKind::Unevaluated(a_uv), ty::ConstKind::Unevaluated(b_uv)) => {
                    a_uv == b_uv
                }
                // FIXME(generic_const_exprs): We may want to either actually try
                // to evaluate `a_ct` and `b_ct` if they are are fully concrete or something like
                // this, for now we just return false here.
                _ => false,
            }
        }
        (Node::Binop(a_op, al, ar), Node::Binop(b_op, bl, br)) if a_op == b_op => {
            try_unify(tcx, a.subtree(al), b.subtree(bl))
                && try_unify(tcx, a.subtree(ar), b.subtree(br))
        }
        (Node::UnaryOp(a_op, av), Node::UnaryOp(b_op, bv)) if a_op == b_op => {
            try_unify(tcx, a.subtree(av), b.subtree(bv))
        }
        (Node::FunctionCall(a_f, a_args), Node::FunctionCall(b_f, b_args))
            if a_args.len() == b_args.len() =>
        {
            try_unify(tcx, a.subtree(a_f), b.subtree(b_f))
                && iter::zip(a_args, b_args)
                    .all(|(&an, &bn)| try_unify(tcx, a.subtree(an), b.subtree(bn)))
        }
        (Node::Cast(a_cast_kind, a_operand, a_ty), Node::Cast(b_cast_kind, b_operand, b_ty))
            if (a_ty == b_ty) && (a_cast_kind == b_cast_kind) =>
        {
            try_unify(tcx, a.subtree(a_operand), b.subtree(b_operand))
        }
        _ => false,
    }
}
