use std::{
    fmt,
    hash::{Hash, Hasher},
};

use gc_arena::{unsize, Collect, Gc, Mutation};

use crate::{Error, Function, Stack};

#[derive(Collect)]
#[collect(no_drop)]
pub enum CallbackReturn<'gc> {
    Return,
    Yield(Option<AnyContinuation<'gc>>),
    TailCall(Function<'gc>, Option<AnyContinuation<'gc>>),
    Sequence(AnySequence<'gc>),
}

pub trait Sequence<'gc>: Collect {
    fn step(
        &mut self,
        mc: &Mutation<'gc>,
        stack: &mut Stack<'gc>,
    ) -> Result<Option<CallbackReturn<'gc>>, Error<'gc>>;
}

#[derive(Collect)]
#[collect(no_drop)]
pub struct AnySequence<'gc>(pub Box<dyn Sequence<'gc> + 'gc>);

impl<'gc, S> From<S> for AnySequence<'gc>
where
    S: Sequence<'gc> + 'gc,
{
    fn from(v: S) -> Self {
        Self(Box::new(v))
    }
}

impl<'gc> AnySequence<'gc> {
    pub fn new(sequence: impl Sequence<'gc> + 'gc) -> Self {
        Self(Box::new(sequence))
    }

    pub fn step(
        &mut self,
        mc: &Mutation<'gc>,
        stack: &mut Stack<'gc>,
    ) -> Result<Option<CallbackReturn<'gc>>, Error<'gc>> {
        self.0.step(mc, stack)
    }
}

pub trait Callback<'gc>: Collect {
    fn call(
        &self,
        mc: &Mutation<'gc>,
        stack: &mut Stack<'gc>,
    ) -> Result<CallbackReturn<'gc>, Error<'gc>>;
}

// Represents a callback as a single pointer with an inline VTable header.
#[derive(Copy, Clone, Collect)]
#[collect(no_drop, bound = "")]
pub struct AnyCallback<'gc>(Gc<'gc, Header<'gc>>);

struct Header<'gc> {
    call: unsafe fn(
        *const (),
        &Mutation<'gc>,
        &mut Stack<'gc>,
    ) -> Result<CallbackReturn<'gc>, Error<'gc>>,
}

impl<'gc> AnyCallback<'gc> {
    pub fn new<C: Callback<'gc> + 'gc>(mc: &Mutation<'gc>, callback: C) -> Self {
        #[repr(C)]
        struct HeaderCallback<'gc, C> {
            header: Header<'gc>,
            callback: C,
        }

        // SAFETY: We can't auto-implement `Collect` due to the function pointer lifetimes, but
        // function pointers can't hold any data. It would be nice if function pointers could have
        // higher rank `for<'gc>` lifetimes.
        unsafe impl<'gc, C: Collect> Collect for HeaderCallback<'gc, C> {
            fn needs_trace() -> bool
            where
                Self: Sized,
            {
                C::needs_trace()
            }

            fn trace(&self, cc: &gc_arena::Collection) {
                self.callback.trace(cc)
            }
        }

        let hc = Gc::new(
            mc,
            HeaderCallback {
                header: Header {
                    call: |ptr, mc, stack| unsafe {
                        let hc = ptr as *const HeaderCallback<C>;
                        ((*hc).callback).call(mc, stack)
                    },
                },
                callback,
            },
        );

        Self(unsafe { Gc::cast::<Header>(hc) })
    }

    pub fn from_fn<F>(mc: &Mutation<'gc>, call: F) -> AnyCallback<'gc>
    where
        F: 'static + Fn(&Mutation<'gc>, &mut Stack<'gc>) -> Result<CallbackReturn<'gc>, Error<'gc>>,
    {
        Self::from_fn_with(mc, (), move |_, mc, stack| call(mc, stack))
    }

    pub fn from_fn_with<C, F>(mc: &Mutation<'gc>, context: C, call: F) -> AnyCallback<'gc>
    where
        C: 'gc + Collect,
        F: 'static
            + Fn(&C, &Mutation<'gc>, &mut Stack<'gc>) -> Result<CallbackReturn<'gc>, Error<'gc>>,
    {
        #[derive(Collect)]
        #[collect(no_drop)]
        struct ContextCallback<C, F> {
            context: C,
            #[collect(require_static)]
            call: F,
        }

        impl<'gc, C, F> Callback<'gc> for ContextCallback<C, F>
        where
            C: 'gc + Collect,
            F: 'static
                + Fn(&C, &Mutation<'gc>, &mut Stack<'gc>) -> Result<CallbackReturn<'gc>, Error<'gc>>,
        {
            fn call(
                &self,
                mc: &Mutation<'gc>,
                stack: &mut Stack<'gc>,
            ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
                (self.call)(&self.context, mc, stack)
            }
        }

        AnyCallback::new(mc, ContextCallback { context, call })
    }

    pub fn as_ptr(self) -> *const () {
        Gc::as_ptr(self.0) as *const ()
    }

    pub fn call(
        self,
        mc: &Mutation<'gc>,
        stack: &mut Stack<'gc>,
    ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
        unsafe { (self.0.call)(Gc::as_ptr(self.0) as *const (), mc, stack) }
    }
}

impl<'gc> fmt::Debug for AnyCallback<'gc> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_tuple("Callback").field(&self.as_ptr()).finish()
    }
}

impl<'gc> PartialEq for AnyCallback<'gc> {
    fn eq(&self, other: &AnyCallback<'gc>) -> bool {
        self.as_ptr() == other.as_ptr()
    }
}

impl<'gc> Eq for AnyCallback<'gc> {}

impl<'gc> Hash for AnyCallback<'gc> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_ptr().hash(state)
    }
}

pub trait Continuation<'gc>: Collect {
    fn continue_ok(
        &self,
        mc: &Mutation<'gc>,
        stack: &mut Stack<'gc>,
    ) -> Result<CallbackReturn<'gc>, Error<'gc>>;

    fn continue_err(
        &self,
        mc: &Mutation<'gc>,
        stack: &mut Stack<'gc>,
        error: Error<'gc>,
    ) -> Result<CallbackReturn<'gc>, Error<'gc>>;
}

#[derive(Clone, Copy, Collect)]
#[collect(no_drop)]
pub struct AnyContinuation<'gc>(pub Gc<'gc, dyn Continuation<'gc>>);

impl<'gc> AnyContinuation<'gc> {
    pub fn new(mc: &Mutation<'gc>, continuation: impl Continuation<'gc> + 'gc) -> Self {
        Self(unsize!(Gc::new(mc, continuation) => dyn Continuation<'gc>))
    }

    pub fn from_fns<FO, FE>(
        mc: &Mutation<'gc>,
        continue_ok: FO,
        continue_err: FE,
    ) -> AnyContinuation<'gc>
    where
        FO: 'static
            + Fn(&Mutation<'gc>, &mut Stack<'gc>) -> Result<CallbackReturn<'gc>, Error<'gc>>,
        FE: 'static
            + Fn(
                &Mutation<'gc>,
                &mut Stack<'gc>,
                Error<'gc>,
            ) -> Result<CallbackReturn<'gc>, Error<'gc>>,
    {
        Self::from_fns_with(
            mc,
            (),
            move |_, mc, stack| continue_ok(mc, stack),
            move |_, mc, stack, error| continue_err(mc, stack, error),
        )
    }

    pub fn from_fns_with<C, FO, FE>(
        mc: &Mutation<'gc>,
        context: C,
        continue_ok: FO,
        continue_err: FE,
    ) -> AnyContinuation<'gc>
    where
        C: 'gc + Collect,
        FO: 'static
            + Fn(&C, &Mutation<'gc>, &mut Stack<'gc>) -> Result<CallbackReturn<'gc>, Error<'gc>>,
        FE: 'static
            + Fn(
                &C,
                &Mutation<'gc>,
                &mut Stack<'gc>,
                Error<'gc>,
            ) -> Result<CallbackReturn<'gc>, Error<'gc>>,
    {
        #[derive(Collect)]
        #[collect(no_drop)]
        struct ContextContinuation<C, FO, FE> {
            context: C,
            #[collect(require_static)]
            continue_ok: FO,
            #[collect(require_static)]
            continue_err: FE,
        }

        impl<'gc, C, FO, FE> Continuation<'gc> for ContextContinuation<C, FO, FE>
        where
            C: 'gc + Collect,
            FO: 'static
                + Fn(&C, &Mutation<'gc>, &mut Stack<'gc>) -> Result<CallbackReturn<'gc>, Error<'gc>>,
            FE: 'static
                + Fn(
                    &C,
                    &Mutation<'gc>,
                    &mut Stack<'gc>,
                    Error<'gc>,
                ) -> Result<CallbackReturn<'gc>, Error<'gc>>,
        {
            fn continue_ok(
                &self,
                mc: &Mutation<'gc>,
                stack: &mut Stack<'gc>,
            ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
                (self.continue_ok)(&self.context, mc, stack)
            }

            fn continue_err(
                &self,
                mc: &Mutation<'gc>,
                stack: &mut Stack<'gc>,
                error: Error<'gc>,
            ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
                (self.continue_err)(&self.context, mc, stack, error)
            }
        }

        AnyContinuation(unsize!(Gc::new(
            mc,
            ContextContinuation {
                context,
                continue_ok,
                continue_err,
            }
        ) => dyn Continuation<'gc>))
    }

    pub fn from_ok_fn<F>(mc: &Mutation<'gc>, continue_ok: F) -> AnyContinuation<'gc>
    where
        F: 'static + Fn(&Mutation<'gc>, &mut Stack<'gc>) -> Result<CallbackReturn<'gc>, Error<'gc>>,
    {
        Self::from_fns_with(
            mc,
            (),
            move |_, mc, stack| continue_ok(mc, stack),
            move |_, _, _, error| Err(error),
        )
    }

    pub fn from_ok_fn_with<C, F>(
        mc: &Mutation<'gc>,
        context: C,
        continue_ok: F,
    ) -> AnyContinuation<'gc>
    where
        C: Collect + 'gc,
        F: 'static
            + Fn(&C, &Mutation<'gc>, &mut Stack<'gc>) -> Result<CallbackReturn<'gc>, Error<'gc>>,
    {
        Self::from_fns_with(
            mc,
            context,
            move |context, mc, stack| continue_ok(context, mc, stack),
            move |_, _, _, error| Err(error),
        )
    }

    pub fn continue_ok(
        &self,
        mc: &Mutation<'gc>,
        stack: &mut Stack<'gc>,
    ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
        self.0.continue_ok(mc, stack)
    }

    pub fn continue_err(
        &self,
        mc: &Mutation<'gc>,
        stack: &mut Stack<'gc>,
        error: Error<'gc>,
    ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
        self.0.continue_err(mc, stack, error)
    }
}

#[cfg(test)]
mod tests {
    use crate::CallbackReturn;

    use gc_arena::rootless_arena;

    use super::*;

    #[test]
    fn test_dyn_callback() {
        rootless_arena(|mc| {
            #[derive(Collect)]
            #[collect(require_static)]
            struct CB(i64);

            impl<'gc> Callback<'gc> for CB {
                fn call(
                    &self,
                    mc: &Mutation<'gc>,
                    stack: &mut Stack<'gc>,
                ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
                    stack.into_front(mc, self.0);
                    Ok(CallbackReturn::Return)
                }
            }

            let dyn_callback = AnyCallback::new(mc, CB(17));

            let mut stack = Stack::new();
            assert!(dyn_callback.call(mc, &mut stack).is_ok());
            assert!(matches!(stack.from_front(mc).unwrap(), 17));
        })
    }
}
