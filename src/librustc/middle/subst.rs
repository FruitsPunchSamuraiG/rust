// Copyright 2012 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Type substitutions.

use middle::ty;
use middle::ty_fold;
use middle::ty_fold::{TypeFoldable, TypeFolder};
use util::ppaux::Repr;

use std::vec::Vec;
use syntax::codemap::Span;

///////////////////////////////////////////////////////////////////////////

/**
 * Represents the values to use when substituting lifetime parameters.
 * If the value is `ErasedRegions`, then this subst is occurring during
 * trans, and all region parameters will be replaced with `ty::ReStatic`. */
#[deriving(Clone, PartialEq, Eq, Hash)]
pub enum RegionSubsts {
    ErasedRegions,
    NonerasedRegions(Vec<ty::Region>)
}

/**
 * The type `Substs` represents the kinds of things that can be substituted to
 * convert a polytype into a monotype.  Note however that substituting bound
 * regions other than `self` is done through a different mechanism:
 *
 * - `tps` represents the type parameters in scope.  They are indexed
 *   according to the order in which they were declared.
 *
 * - `self_r` indicates the region parameter `self` that is present on nominal
 *   types (enums, structs) declared as having a region parameter.  `self_r`
 *   should always be none for types that are not region-parameterized and
 *   Some(_) for types that are.  The only bound region parameter that should
 *   appear within a region-parameterized type is `self`.
 *
 * - `self_ty` is the type to which `self` should be remapped, if any.  The
 *   `self` type is rather funny in that it can only appear on traits and is
 *   always substituted away to the implementing type for a trait. */
#[deriving(Clone, PartialEq, Eq, Hash)]
pub struct Substs {
    pub self_ty: Option<ty::t>,
    pub tps: Vec<ty::t>,
    pub regions: RegionSubsts,
}

impl Substs {
    pub fn empty() -> Substs {
        Substs {
            self_ty: None,
            tps: Vec::new(),
            regions: NonerasedRegions(Vec::new())
        }
    }

    pub fn trans_empty() -> Substs {
        Substs {
            self_ty: None,
            tps: Vec::new(),
            regions: ErasedRegions
        }
    }

    pub fn is_noop(&self) -> bool {
        let regions_is_noop = match self.regions {
            ErasedRegions => false, // may be used to canonicalize
            NonerasedRegions(ref regions) => regions.is_empty()
        };

        self.tps.len() == 0u &&
            regions_is_noop &&
            self.self_ty.is_none()
    }

    pub fn self_ty(&self) -> ty::t {
        self.self_ty.unwrap()
    }
}

///////////////////////////////////////////////////////////////////////////
// Public trait `Subst`
//
// Just call `foo.subst(tcx, substs)` to perform a substitution across
// `foo`. Or use `foo.subst_spanned(tcx, substs, Some(span))` when
// there is more information available (for better errors).

pub trait Subst {
    fn subst(&self, tcx: &ty::ctxt, substs: &Substs) -> Self {
        self.subst_spanned(tcx, substs, None)
    }

    fn subst_spanned(&self, tcx: &ty::ctxt,
                     substs: &Substs,
                     span: Option<Span>)
                     -> Self;
}

impl<T:TypeFoldable> Subst for T {
    fn subst_spanned(&self,
                     tcx: &ty::ctxt,
                     substs: &Substs,
                     span: Option<Span>)
                     -> T
    {
        let mut folder = SubstFolder { tcx: tcx,
                                       substs: substs,
                                       span: span,
                                       root_ty: None,
                                       ty_stack_depth: 0 };
        (*self).fold_with(&mut folder)
    }
}

///////////////////////////////////////////////////////////////////////////
// The actual substitution engine itself is a type folder.

struct SubstFolder<'a> {
    tcx: &'a ty::ctxt,
    substs: &'a Substs,

    // The location for which the substitution is performed, if available.
    span: Option<Span>,

    // The root type that is being substituted, if available.
    root_ty: Option<ty::t>,

    // Depth of type stack
    ty_stack_depth: uint,
}

impl<'a> TypeFolder for SubstFolder<'a> {
    fn tcx<'a>(&'a self) -> &'a ty::ctxt { self.tcx }

    fn fold_region(&mut self, r: ty::Region) -> ty::Region {
        // Note: This routine only handles regions that are bound on
        // type declarations and other outer declarations, not those
        // bound in *fn types*. Region substitution of the bound
        // regions that appear in a function signature is done using
        // the specialized routine
        // `middle::typeck::check::regionmanip::replace_late_regions_in_fn_sig()`.
        match r {
            ty::ReEarlyBound(_, i, _) => {
                match self.substs.regions {
                    ErasedRegions => ty::ReStatic,
                    NonerasedRegions(ref regions) => *regions.get(i),
                }
            }
            _ => r
        }
    }

    fn fold_ty(&mut self, t: ty::t) -> ty::t {
        if !ty::type_needs_subst(t) {
            return t;
        }

        // track the root type we were asked to substitute
        let depth = self.ty_stack_depth;
        if depth == 0 {
            self.root_ty = Some(t);
        }
        self.ty_stack_depth += 1;

        let t1 = match ty::get(t).sty {
            ty::ty_param(p) => {
                // FIXME -- This...really shouldn't happen. We should
                // never be substituting without knowing what's in
                // scope and knowing that the indices will line up!
                if p.idx < self.substs.tps.len() {
                    *self.substs.tps.get(p.idx)
                } else {
                    let root_msg = match self.root_ty {
                        Some(root) => format!(" in the substitution of `{}`",
                                              root.repr(self.tcx)),
                        None => "".to_string()
                    };
                    let m = format!("can't use type parameters from outer \
                                    function{}; try using a local type \
                                    parameter instead",
                                    root_msg);
                    match self.span {
                        Some(span) => {
                            self.tcx.sess.span_err(span, m.as_slice())
                        }
                        None => self.tcx.sess.err(m.as_slice())
                    }
                    ty::mk_err()
                }
            }
            ty::ty_self(_) => {
                match self.substs.self_ty {
                    Some(ty) => ty,
                    None => {
                        let root_msg = match self.root_ty {
                            Some(root) => format!(" in the substitution of `{}`",
                                                  root.repr(self.tcx)),
                            None => "".to_string()
                        };
                        let m = format!("missing `Self` type param{}",
                                        root_msg);
                        match self.span {
                            Some(span) => {
                                self.tcx.sess.span_err(span, m.as_slice())
                            }
                            None => self.tcx.sess.err(m.as_slice())
                        }
                        ty::mk_err()
                    }
                }
            }
            _ => ty_fold::super_fold_ty(self, t)
        };

        assert_eq!(depth + 1, self.ty_stack_depth);
        self.ty_stack_depth -= 1;
        if depth == 0 {
            self.root_ty = None;
        }

        t1
    }
}
