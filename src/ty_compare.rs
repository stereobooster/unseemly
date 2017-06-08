use ast_walk::{walk, LazyWalkReses, WalkMode, WalkRule};
use ast_walk::WalkRule::*;
use form::Form;
use util::assoc::Assoc;
use ast::*;
use util::mbe::EnvMBE;
use ty::{Ty, TyErr, TypeError};
use name::*;
use std::cell::{Ref,RefCell};
use core_forms::{find_core_form, ast_to_atom};

/* Let me write down an example subtyping hierarchy, to stop myself from getting confused.
    ⊤ (any type/dynamic type/"dunno"/∀X.X)
   ╱              |                       |          ╲
 Num          ∀X Y.(X⇒Y)               Nat⇒Int     ∀X Y.(X,Y)
  |           ╱         ╲              ╱     ╲         ╲
 Int     ∀Y.(Bool⇒Y)  ∀X.(X⇒Bool)  Int⇒Int  Nat⇒Nat  ∀X.(X, Bool)
  |           ╲         ╱              ╲     ╱           |
 Nat           Bool⇒Bool               Int⇒Nat      (Nat,Bool)
   ╲               |                      |            ╱
    ⊥ (uninhabited type/panic/"can't happen"/enum{})
    
How do we see if S is a subtype of T?
First, we positively walk S, turning `∀X.(X⇒X)` into `(G23⇒G23)` 
 (where `G23` is a generated type name),
 producing SArbitrary
Then, we negatively walk T, with SArbitrary as context, similarly eliminating `∀`.
We use side-effects to see if generated type names in T can be consistently assigned 
 to make everything match.

Is (Int, Nat) <: ∀X. (X, X)? 
If so, we could instantiate every type variable at ⊤, eliminating all constraints!
Eliminating ⊤ doesn't prevent (Bool⇒Bool, String⇒String) <: ∀X. (X X), via X=∀Y. Y⇒Y.
I think that this means we need to constrain ∀-originated variables to being equal,
 not subtypes.

Okay, we know that negative positions have the opposite subtyping relationship...

<digression about something not currently implemented>

...weirdly, this kinda suggests that there's an alternative formulation of `∀` 
 that's more concise, and might play better with our system,
 and (for better or worse) can't express certain "exotic" types:
id: ∀X ⇒ X
map: List<[∀X]< (X ⇒ ∀Y) ⇒ List<[Y]<   (need `letrec`-style binding!)
boring_map: List<[Int]< (Int ⇒ ∀Y) ⇒ List<[Y]<    (need `∀` to distinguish binders and refs!)
boring_map2: List<[∀X]< List<[X]< (X X ⇒ ∀Y) ⇒ List<[Y]<
let_macro: "[let :::[ ;[var ⇑ v]; = ;[ expr<[∀T]< ]; ]::: in ;[expr<[∀S]< ↓ ...{v = T}...]; ]"
              -> expr<[S]<

Okay, let's walk through this. Let's suppose that we have some type variables in scope:
 is `(A ⇒ B) ⇒ ((∀A ⇒ F) ⇒ D)` a subtype of `(∀EE ⇒ BB) ⇒ (CC ⇒ EE)`?

It starts as a negative walk of the purported supertype. Destructuring succeeds. 
Add ∀ed type variables to an environment. Now `∀X` might as well be `X`.
 - is [A]`((A ⇒ F) ⇒ D)` a subtype of [EE]`(CC ⇒ EE)`? Destructuring succeeds.
   - is [A]`D` a subtype of [EE]`EE`? Set EE := `D`.
   - is [EE]`CC` a subtype of [A]`(A ⇒ F)`? Depends on `CC`.
       Assuming CC is `CC_arg ⇒ CC_ret`, we set A := CC_arg.
 - is [EE]`(EE ⇒ BB)` a subtype of [A]`(A ⇒ B)`? Destructuring succeeds.
   - is [EE]`BB` a subtype of [A]`B`? Depends on the environment.
   - is [A]`A` a subtype of [EE]`EE`? Both have already been set, so:
     - does `CC_arg` equal `D`? Depends on the environment.
     
What if we re-order the side-effects?
    ⋮
   - is [A]`A` a subtype of [EE]`EE`? Set A := `A_and_EE` and EE := `A_and_EE`.
       (What happens when names escape the scope that defined them??)
    ⋮
   - is [A]`D` a subtype of [EE]`EE`? EE is set to `A_and_EE`, so set A_and_EE := `D`
   - is [EE]`CC` a subtype of [A]`(A ⇒ F)`? Depends on `CC`.
       Assuming CC is `CC_arg ⇒ CC_ret`, does `D` equal `CC_arg`?.
       
Note that, if we allowed ∀ed type variables to participate in subtyping, 
 these two orders would demand opposite relationships between `D` and `CC_arg`.



So, we have this negative/positive distinction. Consider:
  Nat (Int => String) => (∀X ⇒ X) 
If you count how many negations each type is under, 
 you get a picture of the inputs and outputs of the type at a high level.
So, the type needs a `Nat` and a `String` and an `X`, and provides an `Int` and an `X`
 (The `Int` is doubly-negated; the function promises to provide it to the function it is passed.).

What about `Nat (∀X => Nat) => X`, assuming that we have access to `transmogrify`?
When we typecheck an invocation of it, we expect to know the exact type of its arguments,
 but that exact type might well still be `∀X ⇒ Nat`, 
 meaning we have no idea what type we'll return, and no `∀`s left to explain the lack of knowledge.

</digression>

But let's not do that weird thing just yet.

*/


thread_local! {
    static next_id: RefCell<u32> = RefCell::new(0);
    static unification: RefCell<::std::collections::HashMap<Name, Option<Ty>>>
        = RefCell::new(::std::collections::HashMap::new());
}
/*
// TODO: underspecification needs to not be a `type_by_name`, so we can simplify this
pub fn unify_or_lookup(name: Name, t: &Ty, env: Assoc<Name, Ty>) -> Result<(), TyErr> {
    print!("UOL: {:?} {:?}\n", name, t);
    use std::collections::hash_map::Entry::*;
    
    unification.with(|unif| {
        let (lookup_res, sure_to_match) = match unif.borrow_mut().entry(name) {
            // It is or was underspecified...
            Occupied(ref mut occ) => {
                if let &Some(ref occ_t) = occ.get() { // ...it's already specified
                    (occ_t.clone(), false)
                } else {
                     // ...specify it to be this (later).
                    (t.clone(), true)
                }
            }
            Vacant(_) => { // Can't unify; this is a normal type variable
                (env.find_or_panic(&name).clone(), false)
            }
        };
        
        if sure_to_match { 
            //logically belongs at `true`, above, but borrow-checker doesn't like it:
            unif.borrow_mut().insert(name, Some(t.clone())); 
            return Ok(());
        }
        
        // try the same thing on other side...
        let (lookup_res_rhs, sure_to_match) = if let Node(t_form, t_parts) = t.concrete() {
            if t_form == find_core_form("type", "type_by_name") {
                let n_rhs = ast_to_atom(t_parts.get_leaf_or_panic(&n("name")));
                match unif.borrow_mut().entry(n_rhs) {
                    Occupied(ref mut occ) => {
                        if let &Some(ref occ_t) = occ.get() {
                            (occ_t.clone(), false)
                        } else {
                            (lookup_res.clone(), true)
                        }
                    }
                    Vacant(_) => {
                        (env.find_or_panic(&n_rhs).clone(), false)
                    }
                }
            } else {
                (t.clone(), false)
            }
        } else {
            (t.clone(), false)
        };
        
        if sure_to_match { 
            match t.concrete() { // ACK!
                Node(_, t_parts) => {
                    unif.borrow_mut().insert(ast_to_atom(t_parts.get_leaf_or_panic(&n("name"))), 
                                             Some(lookup_res.clone()));                    
                }
                _ => { panic!("can't happen")}
            }
            return Ok(());
        }
        
        print!("UOL:ME: {} {}\n", lookup_res, lookup_res_rhs);
        
        must_equal(&lookup_res, &lookup_res_rhs, env)
    })
}
*/
impl ::ast_walk::WalkElt<Canonicalize> for Ty {
    type Err = ::ty::TyErr;
    
    // Report an error
    fn mismatch_error(got: Ty, expd: Ty) -> Self::Err { TyErr::Mismatch(got, expd) }
    
    fn get_bidi_walk_rule(f: &Form) -> &::form::BiDiWR<Canonicalize, Subtype> {
        &f.type_compare
    }
    fn automatically_extend_env() -> bool { true }
    fn underspecified() -> Ty {
        unification.with(|unif| {
            next_id.with(|id| {
                
                *id.borrow_mut() += 1;
                // TODO: we need `gensym`!
                let new_name = n(("⚁ ".to_string() + id.borrow().to_string().as_str()).as_str());

                print!("###U### {:?}\n", new_name);
                
                unif.borrow_mut().insert(new_name, None); // leave it underspecified for now

                // TODO: this isn't really a variable reference, like `type_by_name` suggests,
                //  since it's not in the environment, 
                //   but needs to be "looked up" with `unify_or_lookup`.
                // Make this better somehow.

                ty!({ "type" "type_by_name" : 
                    "name" => (, ::ast::Atom(new_name))})
                
            })            
        })
    }
    
    fn from_ast(a: &Ast) -> Ty { Ty::new(a.clone()) }
    fn to_ast(&self) -> Ast { self.concrete() }
}

/*
impl ????? for Subtype {
    fn pre_match(got: Self::Elt, expd: Self::Elt) -> Self::Elt {
        panic!("not implemented for this mode")
    }
}
*/

custom_derive!{
    #[derive(Copy, Clone, Debug, Reifiable)]
    pub struct TyCompWalk {}
}
// TODO: does `Canonicalize` do anything that `Typesynth` on types doesn't do?
pub type Canonicalize = ::ast_walk::PositiveWalkMode<Ty, TyCompWalk>;
pub type Subtype = ::ast_walk::NegativeWalkMode<Ty, TyCompWalk>;

pub fn must_subtype(sub: &Ty, sup: &Ty, env: Assoc<Name, Ty>) -> Result<(), TyErr> {
    // TODO: I think we should be canonicalizing first...
    // TODO: they might need different environments?
    let lwr_env = &LazyWalkReses::<Subtype>::new_wrapper(env).with_context(sub.clone());
        
    walk::<Subtype>(&sup.concrete(), lwr_env).map(|_| ())
}

// TODO: I think we need to route some other things (especially in macros.rs) through this...
pub fn must_equal(lhs: &Ty, rhs: &Ty, env: Assoc<Name, Ty>) -> Result<(), TyErr> {
    let lwr_env = &LazyWalkReses::new_wrapper(env);
    if walk::<Canonicalize>(&lhs.concrete(), &lwr_env) 
       == walk::<Canonicalize>(&rhs.concrete(), &lwr_env) {
        Ok(())
    } else {
        Err(TyErr::Mismatch(lhs.clone(), rhs.clone()))
    }
}

#[test]
fn basic_subtyping() {
    use ::ty::TyErr::*;
    use ::util::assoc::Assoc;
    
    fn tbn(nm: &'static str) -> Ty {
        ty!( { "type" "type_by_name" : "name" => (, ::ast::Ast::Atom(n(nm))) } )
    }
    
    let mt_ty_env = Assoc::new();
    let int_ty = ty!({ "type" "int" : });
    let nat_ty = ty!({ "type" "nat" : });
    let bool_ty = ty!({ "type" "bool" : });
    
    
    assert_eq!(must_subtype(&int_ty, &int_ty, mt_ty_env.clone()), Ok(()));

    assert_eq!(must_subtype(&bool_ty, &int_ty, mt_ty_env.clone()), 
        Err(Mismatch(bool_ty.clone(), int_ty.clone())));
                            
    let id_fn_ty = ty!({ "type" "forall_type" :
        "param" => ["t"],
        "body" => (import [* [forall "param"]]
            { "type" "fn" :
                "param" => [ (, tbn("t").concrete()) ],
                "ret" => (, tbn("t").concrete()) })});

    let int_to_int_fn_ty = ty!({ "type" "fn" : 
         "param" => [(, int_ty.concrete())], 
         "ret" => (, int_ty.concrete())});

    assert_eq!(must_subtype(&int_to_int_fn_ty, &int_to_int_fn_ty, mt_ty_env.clone()),
               Ok(()));

    assert_eq!(must_subtype(&id_fn_ty, &id_fn_ty, mt_ty_env.clone()),
               Ok(()));

/*

    // actually subtype interestingly!
    assert_eq!(must_subtype(&int_to_int_fn_ty, &id_fn_ty, mt_ty_env.clone()),
               Ok(()));

    // TODO: this error spits out generated names to the user without context ) :
    assert_m!(must_subtype(&id_fn_ty, &int_to_int_fn_ty, mt_ty_env.clone()),
              Err(Mismatch(_,_)));
              
    let parametric_ty_env = assoc_n!(
        "some_int" => ty!( { "type" "int" : }),
        "convert_to_nat" => ty!({ "type" "forall_type" :
            "param" => ["t"],
            "body" => (import [* [forall "param"]] 
                { "type" "fn" :
                    "param" => [ (, tbn("t").concrete() ) ],
                    "ret" => (, nat_ty.concrete() ) })}),
        "identity" => id_fn_ty.clone(),
        "int_to_int" => int_to_int_fn_ty.clone());

              
    assert_eq!(must_subtype(&tbn("int_to_int"), &tbn("identity"), parametric_ty_env.clone()),
              Ok(()));
              
    assert_m!(must_subtype(&tbn("identity"), &tbn("int_to_int"), parametric_ty_env.clone()),
              Err(Mismatch(_,_)));
*/    

}