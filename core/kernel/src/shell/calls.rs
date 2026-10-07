//! What other threads ask of the interface thread: calls and their answers, events, and the jobs
//! that ended.

use super::*;

impl Shell {
    /// Moves what other threads left for the interface thread: events and failures of commands.
    pub(super) fn collect_from_threads(&mut self) {
        let bridge = self.host.bridge.clone();
        let events = std::mem::take(&mut *bridge.events.lock().unwrap_or_else(|e| e.into_inner()));
        self.host.events.extend(events);
        let failures = std::mem::take(&mut *bridge.failures.lock().unwrap_or_else(|e| e.into_inner()));
        self.host.reported.extend(failures);
    }

    /// Hands the jobs that ended back to their modules.
    pub(super) fn deliver_jobs(&mut self) {
        for finished in self.host.pool.take_finished() {
            let Some(index) = self.running_index(&finished.owner) else {
                log::warn!(
                    "job '{}' of '{}' ended after its module stopped",
                    finished.label,
                    finished.owner
                );
                continue;
            };
            let (id, outcome) = (finished.id, finished.outcome);
            let start = Instant::now();
            let done = call_module(&mut self.slots[index], &mut self.host, |f, ctx| {
                f.on_job(id, outcome, ctx)
            });
            uniwow_api::journal::spent(&format!("jobs handed back to {}", finished.owner), start.elapsed());
            if let Err(message) = done {
                self.fail(index, format!("job '{}': {message}", finished.label));
            }
        }
    }

    /// Answers the queued calls for at most `budget` (T4), then delivers the answers due to
    /// modules. Returns whether the time ran out with calls perhaps still waiting.
    pub(super) fn serve_calls(&mut self, budget: Duration) -> bool {
        let Some(requests) = self.requests.take() else {
            return false;
        };
        let out_of_time = router::serve(&requests, budget, CALL_IDLE, |request| self.answer(request));
        self.requests = Some(requests);
        self.apply_reported();
        for (caller, call, result) in std::mem::take(&mut self.replies) {
            let Some(index) = self.running_index(&caller) else {
                continue;
            };
            if let Err(message) = call_module(&mut self.slots[index], &mut self.host, |f, ctx| {
                f.on_reply(call, result, ctx)
            }) {
                self.fail(index, format!("reply: {message}"));
            }
        }
        out_of_time
    }

    pub(super) fn answer(&mut self, request: Request) {
        match request {
            Request::Call {
                caller,
                thread,
                name,
                arguments,
                reply,
            } => {
                // What the caller published before calling goes first.
                self.collect_from_threads();
                if self.host.bridge.active(&caller).is_ok()
                    && let Some((owner, handler, module)) = self.compiled_command(&name)
                {
                    // Its module's thread runs it and answers later: the interface goes on (T4).
                    let bridge = self.host.bridge.clone();
                    uniwow_api::ui::lock(&module.ui).post_job(Box::new(move || {
                        // Its module may have failed meanwhile.
                        let result = bridge
                            .lookup(&name)
                            .and_then(|_| bridge.run_on_caller(&owner, &name, &handler, arguments));
                        bridge.queue(Request::Answer {
                            caller,
                            name,
                            reply,
                            result,
                        });
                    }));
                    return;
                }
                let result = self.run_command(&caller, thread, &name, arguments);
                self.reply(&caller, &name, reply, result);
            }
            Request::Answer {
                caller,
                name,
                reply,
                result,
            } => self.reply(&caller, &name, reply, result),
            Request::BeginGroup { caller, thread, label } => {
                if self.host.bridge.active(&caller).is_ok() {
                    self.groups.begin(&caller, thread, &label);
                }
            }
            Request::EndGroup { caller, thread } => match self.groups.end(&caller, thread) {
                Ended::Closed(closed) => self.push_closed(closed),
                Ended::StillOpen => {}
                Ended::NotOpen => log::warn!("'{caller}' ended an undo group it had not opened"),
            },
            Request::RecordChange {
                caller,
                thread,
                label,
                change,
            } => {
                let owner = router::module_of(&caller).to_owned();
                if self.host.bridge.active(&caller).is_err() || self.running_index(&owner).is_none() {
                    return;
                }
                let part = history::Part {
                    owner,
                    label: label.clone(),
                    document: change.document(),
                    command: Box::new(Recorded { label, change }),
                };
                match self.groups.parts_of(&caller, thread) {
                    Some(parts) => parts.push(part),
                    None => self.history.push(part),
                }
            }
            Request::ThreadEnded { thread } => {
                for closed in self.groups.close_thread(thread) {
                    log::warn!("undo group '{}' closed: its job ended without ending it", closed.label);
                    self.push_closed(closed);
                }
            }
        }
    }

    /// Hands the result of a call to whoever waits for it.
    pub(super) fn reply(
        &mut self,
        caller: &str,
        name: &str,
        reply: ReplyTo,
        result: Result<serde_json::Value, String>,
    ) {
        if let Err(error) = &result {
            log::warn!("call of '{name}' by '{caller}' failed: {error}");
        }
        match reply {
            ReplyTo::Thread(reply) => {
                // The caller may have given up; nothing to do then.
                let _ = reply.send(result);
            }
            ReplyTo::Module(caller, call) => self.replies.push((caller, call, result)),
            ReplyTo::Kernel(call) => self.commands_panel.answer(call, result),
        }
    }

    /// The command `name` when a running compiled module offers it, with its handler and the
    /// module's thread to run it on.
    pub(super) fn compiled_command(
        &self,
        name: &str,
    ) -> Option<(String, CommandHandler, &'static capi::ModuleContext)> {
        let (owner, handler) = self.host.bridge.lookup(name).ok()?;
        let module = self.slots[self.running_index(&owner)?].compiled?;
        Some((owner, handler?, module))
    }

    pub(super) fn run_command(
        &mut self,
        caller: &str,
        thread: ThreadId,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let bridge = self.host.bridge.clone();
        // Asked before the caller's module failed.
        bridge.active(caller)?;
        let (owner, handler) = bridge.lookup(name)?;
        if let Some(handler) = handler {
            return bridge.run_on_caller(&owner, name, &handler, arguments);
        }
        let index = self
            .running_index(&owner)
            .ok_or_else(|| format!("'{name}' belongs to '{owner}', which is not running"))?;
        let slot = &mut self.slots[index];
        let module = slot.module.as_deref_mut().expect("running modules are loaded");
        let mut ctx = Context::for_command(&mut self.host, &slot.id, caller);
        let outcome = guarded_as(&slot.id, || module.on_command(name, arguments, &mut ctx));
        match outcome {
            Ok(result) => {
                self.apply_pending_for(Some((caller, thread)));
                result
            }
            Err(panic) => {
                self.fail(index, format!("command '{name}': {panic}"));
                Err(format!("'{name}' failed: {panic}"))
            }
        }
    }

    /// Delivers the events published this frame. Events published meanwhile wait for the next one.
    pub(super) fn dispatch_events(&mut self) {
        for event in std::mem::take(&mut self.host.events) {
            if event.topic == DIALOG_ANSWERED_TOPIC {
                self.closing_answered(&event);
            }
            self.host.bridge.deliver(&event);
            for index in 0..self.slots.len() {
                let slot = &self.slots[index];
                if !slot.state.is_running() || !slot.subscribed_to(&event.topic) {
                    continue;
                }
                if let Err(message) =
                    call_module(&mut self.slots[index], &mut self.host, |f, ctx| f.on_event(&event, ctx))
                {
                    let topic = &event.topic;
                    self.fail(index, format!("event '{topic}': {message}"));
                }
            }
        }
        self.apply_pending();
    }
}
