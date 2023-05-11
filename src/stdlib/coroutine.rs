use gc_arena::{Collect, MutationContext};

use crate::{
    AnyCallback, BadThreadMode, CallbackMode, CallbackReturn, Root, RuntimeError, Sequence, String,
    Table, Thread, ThreadMode, TypeError, Value,
};

pub fn load_coroutine<'gc>(mc: MutationContext<'gc, '_>, _root: Root<'gc>, env: Table<'gc>) {
    let coroutine = Table::new(mc);

    coroutine
        .set(
            mc,
            "create",
            AnyCallback::from_fn(mc, |mc, stack| {
                let function = match stack.get(0).copied().unwrap_or(Value::Nil) {
                    Value::Function(function) => function,
                    value => {
                        return Err(TypeError {
                            expected: "function",
                            found: value.type_name(),
                        }
                        .into());
                    }
                };

                let thread = Thread::new(mc);
                thread.start_suspended(mc, function).unwrap();
                stack.clear();
                stack.push(thread.into());
                Ok(CallbackReturn::Return.into())
            })
            .into(),
        )
        .unwrap();

    coroutine
        .set(
            mc,
            "resume",
            AnyCallback::from_fn(mc, |mc, stack| {
                let thread = match stack.get(0).copied().unwrap_or(Value::Nil) {
                    Value::Thread(closure) => closure,
                    value => {
                        return Err(TypeError {
                            expected: "thread",
                            found: value.type_name(),
                        }
                        .into());
                    }
                };

                thread.resume(mc, stack.drain(1..)).map_err(|_| {
                    RuntimeError(String::from_static(mc, "cannot resume thread").into())
                })?;

                #[derive(Collect)]
                #[collect(require_static)]
                struct ThreadSequence;

                impl<'gc> Sequence<'gc> for ThreadSequence {
                    fn step(
                        &mut self,
                        mc: MutationContext<'gc, '_>,
                        stack: &mut Vec<Value<'gc>>,
                    ) -> Result<Option<CallbackReturn<'gc>>, crate::Error<'gc>>
                    {
                        let thread = match stack.get(0) {
                            Some(&Value::Thread(thread)) => thread,
                            _ => panic!("thread lost from stack"),
                        };

                        match thread.mode() {
                            ThreadMode::Return => {
                                stack.clear();
                                match thread.take_return(mc).unwrap() {
                                    Ok(res) => {
                                        stack.push(Value::Boolean(true));
                                        stack.extend(res)
                                    }
                                    Err(err) => {
                                        stack.extend([Value::Boolean(false), err.to_value(mc)]);
                                    }
                                }
                                Ok(Some(CallbackReturn::Return))
                            }
                            ThreadMode::Normal => {
                                thread.step(mc).unwrap();
                                Ok(None)
                            }
                            mode => Err(BadThreadMode {
                                expected: ThreadMode::Normal,
                                found: mode,
                            }
                            .into()),
                        }
                    }
                }

                Ok(CallbackMode::Sequence(ThreadSequence.into()))
            })
            .into(),
        )
        .unwrap();

    coroutine
        .set(
            mc,
            "status",
            AnyCallback::from_fn(mc, |mc, stack| {
                let thread = match stack.get(0).copied().unwrap_or(Value::Nil) {
                    Value::Thread(closure) => closure,
                    value => {
                        return Err(TypeError {
                            expected: "thread",
                            found: value.type_name(),
                        }
                        .into());
                    }
                };

                stack.clear();
                stack.push(
                    String::from_static(
                        mc,
                        match thread.mode() {
                            ThreadMode::Stopped | ThreadMode::Return => "dead",
                            ThreadMode::Running => "running",
                            ThreadMode::Normal => "normal",
                            ThreadMode::Suspended => "suspended",
                        },
                    )
                    .into(),
                );
                Ok(CallbackReturn::Return.into())
            })
            .into(),
        )
        .unwrap();

    coroutine
        .set(
            mc,
            "yield",
            AnyCallback::from_fn(mc, |_, _| Ok(CallbackReturn::Yield(None).into())).into(),
        )
        .unwrap();

    env.set(mc, "coroutine", coroutine.into()).unwrap();
}
