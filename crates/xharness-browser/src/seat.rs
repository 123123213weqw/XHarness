//! A presentation subscription never acquires this seat. Only the trusted
//! shell can change it; Agent requests carry an admission generation internally.
#![forbid(unsafe_code)]
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Driver {
    #[default]
    Agent,
    Human,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Ticket {
    pub(super) generation: u64,
    pub(super) driver: Driver,
}

#[derive(Default)]
struct State {
    ticket: Ticket,
    switching: bool,
    closed: bool,
    uncertain_effect: bool,
    needs_agent_observation: bool,
}

#[derive(Default)]
pub(super) struct Seat(Mutex<State>);

impl Seat {
    pub(super) fn snapshot(&self) -> Result<Ticket, String> {
        self.0
            .lock()
            .map(|s| s.ticket)
            .map_err(|_| "browser control unavailable".into())
    }

    pub(super) fn receipt(&self) -> Result<Value, String> {
        let state = self.0.lock().map_err(|_| "browser control unavailable")?;
        Ok(json!({
            "mode": if state.switching { json!("switching") } else { json!(state.ticket.driver) },
            "generation": state.ticket.generation.to_string(), "prior_effect_uncertain":state.uncertain_effect,
            "fresh_observation_required":state.needs_agent_observation
        }))
    }

    pub(super) fn require(&self, ticket: Ticket, driver: Driver) -> Result<(), String> {
        let state = self.0.lock().map_err(|_| "browser control unavailable")?;
        if state.closed || state.switching || state.ticket != ticket || ticket.driver != driver {
            return Err(
                "browser control changed or is held by the user; do not replay this operation"
                    .into(),
            );
        }
        Ok(())
    }

    pub(super) fn retire(&self, agent: Option<Ticket>) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "browser control unavailable")?;
        if let Some(ticket) = agent {
            if state.closed
                || state.switching
                || state.ticket != ticket
                || ticket.driver != Driver::Agent
            {
                return Err("browser close denied because control changed; do not replay".into());
            }
        }
        state.closed = true;
        Ok(())
    }

    pub(super) fn begin(&self, expected: u64, to: Driver) -> Result<Transition<'_>, String> {
        let mut state = self.0.lock().map_err(|_| "browser control unavailable")?;
        if state.closed
            || state.switching
            || state.ticket.generation != expected
            || state.ticket.driver == to
        {
            return Err("stale browser control change; refresh the preview".into());
        }
        state.ticket.generation = expected
            .checked_add(1)
            .ok_or("browser control generation exhausted")?;
        state.switching = true;
        Ok(Transition {
            seat: self,
            to,
            pending: true,
        })
    }

    pub(super) fn confirm_observation(&self, ticket: Ticket) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "browser control unavailable")?;
        if state.closed
            || state.switching
            || state.ticket != ticket
            || ticket.driver != Driver::Agent
        {
            return Err(
                "browser changed while observing; get a fresh observation after return".into(),
            );
        }
        state.needs_agent_observation = false;
        Ok(())
    }

    pub(super) fn effect(&self, ticket: Ticket) -> Result<Effect<'_>, String> {
        let state = self.0.lock().map_err(|_| "browser control unavailable")?;
        if state.closed || state.switching || state.ticket != ticket {
            return Err(
                "browser control changed or is held by the user; do not replay this operation"
                    .into(),
            );
        }
        if ticket.driver == Driver::Agent && state.needs_agent_observation {
            return Err("user returned control; observe the browser again before a write".into());
        }
        Ok(Effect {
            seat: self,
            resolved: false,
        })
    }
}

/// Cancellation restores the last confirmed driver, never its old generation.
/// Any operation queued before the transition must get a new ticket.
pub(super) struct Transition<'a> {
    seat: &'a Seat,
    to: Driver,
    pending: bool,
}
impl Transition<'_> {
    pub(super) fn commit(mut self) -> Result<(), String> {
        let mut state = self
            .seat
            .0
            .lock()
            .map_err(|_| "browser control unavailable")?;
        if state.closed {
            return Err("browser closed while changing control".into());
        }
        state.ticket.driver = self.to;
        state.needs_agent_observation = self.to == Driver::Agent;
        state.switching = false;
        self.pending = false;
        Ok(())
    }
}
impl Drop for Transition<'_> {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        if let Ok(mut state) = self.seat.0.lock() {
            state.switching = false;
        }
    }
}

/// A dropped future cannot certify that an admitted browser write did not run.
pub(super) struct Effect<'a> {
    seat: &'a Seat,
    resolved: bool,
}
impl Effect<'_> {
    pub(super) fn resolve(mut self, known: bool) {
        self.resolved = known;
    }
}
impl Drop for Effect<'_> {
    fn drop(&mut self) {
        if !self.resolved {
            if let Ok(mut state) = self.seat.0.lock() {
                state.uncertain_effect = true;
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Manual {
    Takeover {
        generation: String,
    },
    Return {
        generation: String,
    },
    Click {
        generation: String,
        frame_id: String,
        x: f64,
        y: f64,
    },
    Text {
        generation: String,
        frame_id: String,
        text: String,
    },
    Key {
        generation: String,
        frame_id: String,
        key: String,
    },
    Navigate {
        generation: String,
        url: String,
    },
    Status {},
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_invalidates_stale_agent_and_human_even_after_roundtrip() {
        let seat = Seat::default();
        let agent = seat.snapshot().unwrap();
        let take = seat.begin(agent.generation, Driver::Human).unwrap();
        assert_eq!(seat.receipt().unwrap()["mode"], "switching");
        assert!(seat.require(agent, Driver::Agent).is_err());
        assert!(seat.begin(0, Driver::Human).is_err());
        take.commit().unwrap();
        let human = seat.snapshot().unwrap();
        assert!(seat.require(human, Driver::Human).is_ok());
        seat.begin(human.generation, Driver::Agent)
            .unwrap()
            .commit()
            .unwrap();
        assert!(seat.require(agent, Driver::Agent).is_err());
        assert!(seat.require(human, Driver::Human).is_err());
        assert!(seat
            .require(seat.snapshot().unwrap(), Driver::Agent)
            .is_ok());
    }

    #[test]
    fn cancelled_transition_keeps_driver_without_revalidating_old_work() {
        let seat = Seat::default();
        let old = seat.snapshot().unwrap();
        drop(seat.begin(old.generation, Driver::Human).unwrap());
        assert_eq!(seat.receipt().unwrap()["mode"], "agent");
        assert!(seat.require(old, Driver::Agent).is_err());
        assert_eq!(seat.receipt().unwrap()["generation"], "1");
    }

    #[test]
    fn dropped_admitted_effect_must_not_be_reported_as_unstarted() {
        let seat = Seat::default();
        let agent = seat.snapshot().unwrap();
        seat.effect(agent).unwrap().resolve(true);
        assert_eq!(seat.receipt().unwrap()["prior_effect_uncertain"], false);
        drop(seat.effect(agent).unwrap());
        seat.begin(agent.generation, Driver::Human)
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(seat.receipt().unwrap()["prior_effect_uncertain"], true);
    }
}
