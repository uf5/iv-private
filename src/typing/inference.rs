use super::prelude_types;
use std::collections::HashMap;
use std::collections::HashSet;
use std::iter::once;
use std::iter::zip;
use std::sync::atomic::AtomicUsize;

use super::types::*;
use crate::syntax::ast::*;
use crate::syntax::module_wrapper::ModuleConstrMaps;

#[derive(Debug)]
pub struct InferenceError {
    pub span: Span,
    pub error: InferenceErrorMessage,
}

#[derive(Debug)]
pub enum InferenceErrorMessage {
    AnnInfConflict { inf: OpType, ann: OpType },
    UnificationError { t1: Type, t2: Type },
    UnknownOp { name: String },
    UnknownConstructor { name: String },
    DuplicateConstructor { name: String },
    NotAllConstructorsCovered,
    TypeOrderErrorElem { general: Type, concrete: Type },
    TypeOrderErrorOp { general: OpType, concrete: OpType },
    OpPrePostLenNeq { general: OpType, concrete: OpType },
    OccursCheck { name: String },
    ListMGULengthDifferent,
}

type Subst = HashMap<String, Type>;

fn compose(s1: Subst, s2: Subst) -> Subst {
    let mut s: Subst = s1.into_iter().map(|(v, t)| (v, t.apply(&s2))).collect();
    s.extend(s2);
    s
}

trait Typeable {
    fn ftv(&self) -> HashSet<String>;
    fn apply(&self, subst: &Subst) -> Self;
    fn mgu(t1: &Self, t2: &Self) -> Result<Subst, InferenceErrorMessage>;
}

impl Typeable for Type {
    fn ftv(&self) -> HashSet<String> {
        match self {
            Type::Mono(_) => HashSet::new(),
            Type::Poly(v) => HashSet::from([v.clone()]),
            Type::Op(op_type) => op_type.ftv(),
            Type::App(t1, t2) => {
                let mut f = t1.ftv();
                f.extend(t2.ftv());
                f
            }
        }
    }

    fn apply(&self, subst: &Subst) -> Self {
        match self {
            Type::Mono(_) => self.clone(),
            Type::Poly(v) => match subst.get(v) {
                Some(t) => t.clone(),
                None => Type::Poly(v.to_owned()),
            },
            Type::Op(op_type) => Type::Op(op_type.apply(subst)),
            Type::App(t1, t2) => Type::App(Box::new(t1.apply(subst)), Box::new(t2.apply(subst))),
        }
    }

    fn mgu(t1: &Self, t2: &Self) -> Result<Subst, InferenceErrorMessage> {
        match (t1, t2) {
            (Type::Mono(name1), Type::Mono(name2)) if name1 == name2 => Ok(Subst::new()),
            (Type::Poly(name1), Type::Poly(name2)) if name1 == name2 => Ok(Subst::new()),
            (Type::Poly(v), t) | (t, Type::Poly(v)) => {
                if t.ftv().contains(v) {
                    return Err(InferenceErrorMessage::OccursCheck { name: v.to_owned() });
                }
                Ok(HashMap::from([(v.to_owned(), t.to_owned())]))
            }
            (Type::App(lhs1, rhs1), Type::App(lhs2, rhs2)) => {
                let s1 = Type::mgu(lhs1, lhs2)?;
                let rhs1 = rhs1.apply(&s1);
                let rhs2 = rhs2.apply(&s1);
                let s2 = Type::mgu(&rhs1, &rhs2)?;
                Ok(compose(s1, s2))
            }
            (Type::Op(o1), Type::Op(o2)) => Typeable::mgu(o1, o2),
            (_, _) => Err(InferenceErrorMessage::UnificationError {
                t1: t1.clone(),
                t2: t2.clone(),
            }),
        }
    }
}

impl Typeable for OpType {
    fn ftv(&self) -> HashSet<String> {
        self.pre
            .iter()
            .chain(self.post.iter())
            .flat_map(Typeable::ftv)
            .collect()
    }

    fn apply(&self, subst: &Subst) -> Self {
        let pre = self.pre.iter().map(|t| t.apply(subst)).collect();
        let post = self.post.iter().map(|t| t.apply(subst)).collect();
        OpType { pre, post }
    }

    fn mgu(t1: &Self, t2: &Self) -> Result<Subst, InferenceErrorMessage> {
        let s1 = Typeable::mgu(&t1.pre, &t2.pre)?;
        let t1 = t1.post.apply(&s1);
        let t2 = t2.post.apply(&s1);
        let s2 = Typeable::mgu(&t1, &t2)?;
        Ok(compose(s1, s2))
    }
}

impl<T> Typeable for Vec<T>
where
    T: Typeable + Clone,
{
    fn ftv(&self) -> HashSet<String> {
        self.into_iter().flat_map(Typeable::ftv).collect()
    }

    fn apply(&self, subst: &Subst) -> Self {
        self.iter().map(|x| x.apply(subst)).collect()
    }

    fn mgu(t1: &Self, t2: &Self) -> Result<Subst, InferenceErrorMessage> {
        if t1.len() != t2.len() {
            return Err(InferenceErrorMessage::ListMGULengthDifferent);
        }
        let mut s = Subst::new();
        for (x, y) in zip(t1.into_iter(), t2.into_iter()) {
            let x = x.apply(&s);
            let y = y.apply(&s);
            let ss = Typeable::mgu(&x, &y)?;
            s = compose(s, ss);
        }
        Ok(s)
    }
}

struct ModuleConstrOpTypeMap<'m> {
    pub constr_to_optype_map: HashMap<&'m str, OpType>,
}

impl<'m> ModuleConstrOpTypeMap<'m> {
    pub fn new(module: &'m Module) -> Self {
        let mut constr_to_optype_map = HashMap::new();
        for (data_name, data_def) in module.data_defs.iter() {
            for (constr_name, constr_def) in data_def.constrs.iter() {
                let constructed_type = data_def
                    .params
                    .iter()
                    .map(|p| Type::Poly(p.to_owned()))
                    .fold(Type::Mono(data_name.to_owned()), |a, x| {
                        Type::App(Box::new(a), Box::new(x))
                    });
                let optype = OpType {
                    pre: constr_def.params.clone(),
                    post: vec![constructed_type],
                };
                constr_to_optype_map.insert(constr_name.as_str(), optype);
            }
        }
        ModuleConstrOpTypeMap {
            constr_to_optype_map,
        }
    }
}

pub struct Inference<'m> {
    module: &'m Module,
    constr_maps: ModuleConstrMaps<'m>,
    optype_maps: ModuleConstrOpTypeMap<'m>,
    counter: AtomicUsize,
}

impl<'m> Inference<'m> {
    pub fn new(module: &'m Module) -> Self {
        let constr_maps = ModuleConstrMaps::new(module);
        let optype_maps = ModuleConstrOpTypeMap::new(module);
        Inference {
            module,
            constr_maps,
            optype_maps,
            counter: AtomicUsize::new(0),
        }
    }

    pub fn typecheck(&self) -> Result<(), InferenceError> {
        for (op_name, op_def) in self.module.op_defs.iter() {
            if op_name.starts_with("noc") {
                continue;
            }
            let inf = self.infer(&op_def.body)?;
            let ann_inst = self.instantiate_op(op_def.ann.clone());
            self.inf_vs_ann(inf, &ann_inst)
                .map_err(|error| InferenceError {
                    error,
                    span: op_def.span.clone(),
                })?;
        }
        Ok(())
    }

    fn inf_vs_ann(&self, inf: OpType, ann: &OpType) -> Result<(), InferenceErrorMessage> {
        // augment stacks toward the annotation
        let inf = self.augment_op_ow(inf, ann);
        let s = OpType::mgu(&inf, ann)?;
        // ann matches the inf when all subs associated with ftv of annotation are poly
        for v in ann.ftv().iter().filter_map(|t| s.get(t)) {
            match v {
                Type::Poly(_) => (),
                _ => Err(InferenceErrorMessage::AnnInfConflict {
                    inf: inf.clone(),
                    ann: ann.clone(),
                })?,
            }
        }
        Ok(())
    }

    fn gen_name(&self) -> Type {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let name = format!("_gen_{}", n);
        Type::Poly(name)
    }

    fn instantiate_op(&self, op: OpType) -> OpType {
        let new_var_subst = op.ftv().into_iter().map(|v| (v, self.gen_name())).collect();
        op.apply(&new_var_subst)
    }

    /// Augments the first argument's pre and post stacks towards the target
    fn augment_op_ow(&self, mut general: OpType, concrete: &OpType) -> OpType {
        while general.pre.len() < concrete.pre.len() && general.post.len() < concrete.post.len() {
            let new_var = self.gen_name();
            general.pre.push(new_var.clone());
            general.post.push(new_var.clone());
        }
        general
    }

    /// augment both optypes aso that both optypes have the same stacks lengths
    fn augment_op_bw(&self, o1: OpType, o2: OpType) -> (OpType, OpType) {
        let o1 = self.augment_op_ow(o1, &o2);
        let o2 = self.augment_op_ow(o2, &o1);
        (o1, o2)
    }

    fn lit_optype(&self, lit: &Literal) -> OpType {
        let lit_type = match lit {
            Literal::Int(_) => Type::Mono("Int".to_owned()),
        };
        OpType {
            pre: vec![],
            post: vec![lit_type],
        }
    }

    fn make_destr(constr: &OpType) -> OpType {
        OpType {
            pre: constr.post.clone(),
            post: constr.pre.clone(),
        }
    }

    fn lookup_constructor_optype(&self, name: &str) -> Option<&OpType> {
        self.optype_maps.constr_to_optype_map.get(name)
    }

    fn lookup_constructor_data_def(&self, name: &str) -> Option<&&DataDef> {
        self.constr_maps
            .constr_to_data_map
            .get(name)
            .map(|(_data_name, data_def)| data_def)
    }

    fn infer_case_arm(&self, arm: &CaseArm) -> Result<OpType, InferenceError> {
        let constr_ot = self
            .lookup_constructor_optype(arm.constr.as_str())
            .cloned()
            .ok_or_else(|| InferenceError {
                error: InferenceErrorMessage::UnknownConstructor {
                    name: arm.constr.to_owned(),
                },
                span: arm.span.to_owned(),
            })?;
        let body_optype = self.infer(&arm.body)?;
        // create a destructor from the constructor op type and instantiate it
        let destr = Self::make_destr(&constr_ot);
        let inst_destr = self.instantiate_op(destr);
        // chain the destructor with the arm body to get the complete op type
        self.chain(inst_destr, body_optype)
            .map_err(|error| InferenceError {
                error,
                span: arm.span.to_owned(),
            })
    }

    fn get_prelude_optype(&self, name: &str) -> Option<OpType> {
        prelude_types::get(name)
    }

    fn get_constr_optype(&self, name: &str) -> Option<OpType> {
        self.optype_maps.constr_to_optype_map.get(name).cloned()
    }

    fn get_user_optype(&self, name: &str) -> Option<OpType> {
        self.module
            .op_defs
            .get(name)
            .map(|op_def| &op_def.ann)
            .cloned()
    }

    fn lookup_op_optype(&self, name: &str) -> Option<OpType> {
        // lookup the prelude, constructors, user defined
        self.get_prelude_optype(name)
            .or_else(|| self.get_constr_optype(name))
            .or_else(|| self.get_user_optype(name))
    }

    /// Chain two operator types through unification. This includes overflow and underflow chain.
    fn chain(&self, ot1: OpType, ot2: OpType) -> Result<OpType, InferenceErrorMessage> {
        let OpType {
            pre: alpha,
            post: beta,
        } = ot1;
        let OpType {
            pre: gamma,
            post: delta,
        } = ot2;
        let l = usize::min(beta.len(), gamma.len());
        let s = Vec::mgu(&beta[..l].into(), &gamma[..l].into())?;
        if beta.len() >= gamma.len() {
            // overflow chain
            let beta_skip_gamma = beta.into_iter().skip(gamma.len());
            let pre = alpha.into_iter().collect();
            let post = delta.into_iter().chain(beta_skip_gamma).collect();
            Ok(OpType { pre, post }.apply(&s))
        } else {
            // underflow chain
            let gamma_skip_beta = gamma.into_iter().skip(beta.len());
            let pre = alpha.into_iter().chain(gamma_skip_beta).collect();
            let post = delta.into_iter().collect();
            Ok(OpType { pre, post }.apply(&s))
        }
    }

    fn infer_op(&self, op: &Op) -> Result<OpType, InferenceError> {
        match op {
            Op::Literal { value, .. } => Ok(self.lit_optype(value)),
            Op::Name { value: name, span } => self
                .lookup_op_optype(name)
                .map(|op| self.instantiate_op(op))
                .ok_or_else(|| InferenceErrorMessage::UnknownOp {
                    name: name.to_owned(),
                })
                .map_err(|error| InferenceError {
                    error,
                    span: span.to_owned(),
                }),
            Op::Quote { value, .. } => {
                let quoted_optype = self.infer(value)?;
                Ok(OpType {
                    pre: vec![],
                    post: vec![Type::Op(quoted_optype)],
                })
            }
            Op::Case {
                head_arm,
                arms,
                span,
            } => {
                let matched_data_type = self
                    .lookup_constructor_data_def(&head_arm.constr)
                    .ok_or_else(|| InferenceError {
                        error: InferenceErrorMessage::UnknownConstructor {
                            name: head_arm.constr.to_owned(),
                        },
                        span: span.to_owned(),
                    })?;

                let matched_data_type_constr_names: HashSet<_> =
                    matched_data_type.constrs.keys().collect();
                let covered_constr_names: HashSet<_> = once(&head_arm.constr)
                    .chain(arms.iter().map(|arm| &arm.constr))
                    .collect();

                if matched_data_type_constr_names != covered_constr_names {
                    return Err(InferenceError {
                        error: InferenceErrorMessage::NotAllConstructorsCovered,
                        span: span.to_owned(),
                    });
                }

                let mut head_ot = self.infer_case_arm(head_arm)?;
                for arm in arms {
                    let mut arm_ot = self.infer_case_arm(arm)?;
                    (head_ot, arm_ot) = self.augment_op_bw(head_ot, arm_ot);
                    let s = OpType::mgu(&head_ot, &arm_ot).map_err(|error| InferenceError {
                        error,
                        span: span.to_owned(),
                    })?;
                    head_ot = head_ot.apply(&s);
                }

                Ok(head_ot)
            }
        }
    }

    fn infer(&self, ops: &[Op]) -> Result<OpType, InferenceError> {
        let mut acc = OpType::empty();
        for op in ops {
            let t = self.infer_op(op)?;
            acc = self.chain(acc, t).map_err(|error| InferenceError {
                error,
                span: op.get_span().clone(),
            })?;
        }
        Ok(acc)
    }
}
