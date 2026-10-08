//! Conversation search across hosts: one answer per daemon, merged for the recent sessions pop-up.

use std::collections::HashMap;

use protocol::host::HostId;
use protocol::message::SearchHit;

/// What one daemon said about the current query.
#[derive(Clone, Debug, Default)]
pub struct HostSearch {
    /// Its hits, best first.
    pub hits: Vec<SearchHit>,
    /// Indexing progress in percent while the host builds its index.
    pub progress: Option<u8>,
    /// The host's search does not work (`SearchUnavailable`).
    pub unavailable: bool,
    /// The host has answered the current query (`SearchResults`).
    pub answered: bool,
}

/// The pop-up's view of all hosts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Merged {
    /// Every working host's hits, each tagged with its host.
    pub hits: Vec<SearchHit>,
    /// The lowest progress among hosts that are still indexing.
    pub progress: Option<u8>,
    /// Hosts whose search is unavailable, in `order`.
    pub unavailable: Vec<HostId>,
    /// A queried host has neither answered the current query nor reported its search unavailable.
    pub pending: bool,
}

/// The hosts to ask: this machine first, then the configured hosts that are connected, in settings order.
pub fn query_order(configured: &[String], connected: impl Fn(&HostId) -> bool) -> Vec<HostId> {
    let mut order = vec![HostId::local()];
    order.extend(configured.iter().map(|name| HostId::named(name)).filter(|host| !host.is_local() && connected(host)));
    order
}

/// Merge the answers: hits of `order`'s hosts one after another (each list keeps its order), the lowest progress, the
/// unavailable hosts and whether any host still has to answer. A host without an answer contributes nothing.
pub fn merge(order: &[HostId], answers: &HashMap<HostId, HostSearch>) -> Merged {
    let mut merged = Merged::default();
    for host in order {
        let Some(answer) = answers.get(host) else {
            merged.pending = true;
            continue;
        };
        merged.pending |= !answer.answered && !answer.unavailable;
        if answer.unavailable {
            merged.unavailable.push(host.clone());
            continue;
        }
        merged.hits.extend(answer.hits.iter().cloned().map(|hit| SearchHit { host: host.clone(), ..hit }));
        if let Some(progress) = answer.progress {
            merged.progress = Some(merged.progress.map_or(progress, |lowest| lowest.min(progress)));
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::message::Speaker;

    fn hit(id: &str) -> SearchHit {
        SearchHit {
            session_id: id.into(),
            claude_dir: "/c".into(),
            speaker: Speaker::You,
            snippet: String::new(),
            highlights: vec![],
            host: HostId::local(),
        }
    }

    fn answer(ids: &[&str], progress: Option<u8>, unavailable: bool) -> HostSearch {
        HostSearch { hits: ids.iter().map(|id| hit(id)).collect(), progress, unavailable, answered: true }
    }

    fn ids(merged: &Merged) -> Vec<(String, String)> {
        merged.hits.iter().map(|hit| (hit.host.name().to_string(), hit.session_id.clone())).collect()
    }

    fn two_hosts() -> (Vec<HostId>, HostId, HostId) {
        let (a, b) = (HostId::named("a"), HostId::named("b"));
        (vec![HostId::local(), a.clone(), b.clone()], a, b)
    }

    #[test]
    fn local_hits_come_first_then_hosts_in_order() {
        let (order, a, b) = two_hosts();
        let mut answers = HashMap::new();
        answers.insert(b, answer(&["b1"], None, false));
        answers.insert(a, answer(&["a1"], None, false));
        answers.insert(HostId::local(), answer(&["l1"], None, false));
        assert_eq!(
            ids(&merge(&order, &answers)),
            [("local", "l1"), ("a", "a1"), ("b", "b1")].map(|(h, i)| (h.to_string(), i.to_string()))
        );
    }

    #[test]
    fn each_hosts_list_keeps_its_order() {
        let (order, a, _) = two_hosts();
        let mut answers = HashMap::new();
        answers.insert(a, answer(&["a2", "a1", "a3"], None, false));
        let found: Vec<_> = ids(&merge(&order, &answers)).into_iter().map(|(_, id)| id).collect();
        assert_eq!(found, ["a2", "a1", "a3"]);
    }

    #[test]
    fn progress_is_the_lowest_of_the_indexing_hosts() {
        let (order, a, b) = two_hosts();
        let mut answers = HashMap::new();
        answers.insert(HostId::local(), answer(&[], Some(80), false));
        answers.insert(a.clone(), answer(&[], Some(40), false));
        answers.insert(b.clone(), answer(&[], None, false));
        assert_eq!(merge(&order, &answers).progress, Some(40));
        answers.insert(HostId::local(), answer(&[], None, false));
        answers.insert(a, answer(&[], None, false));
        assert_eq!(merge(&order, &answers).progress, None);
    }

    #[test]
    fn unavailable_hosts_are_listed_in_order_and_their_hits_ignored() {
        let (order, a, b) = two_hosts();
        let mut answers = HashMap::new();
        answers.insert(b.clone(), answer(&["b1"], Some(10), true));
        answers.insert(a.clone(), answer(&["a1"], None, true));
        answers.insert(HostId::local(), answer(&["l1"], None, false));
        let merged = merge(&order, &answers);
        assert_eq!(merged.unavailable, [a, b]);
        assert_eq!(ids(&merged), [("local".to_string(), "l1".to_string())]);
        assert_eq!(merged.progress, None);
    }

    #[test]
    fn hits_carry_their_host() {
        let (order, a, _) = two_hosts();
        let mut answers = HashMap::new();
        answers.insert(a.clone(), answer(&["a1"], None, false));
        assert_eq!(merge(&order, &answers).hits[0].host, a);
    }

    #[test]
    fn query_order_is_local_then_connected_hosts_in_settings_order() {
        let configured = ["lab".to_string(), "devbox".to_string()];
        let order = query_order(&configured, |_| true);
        assert_eq!(order, [HostId::local(), HostId::named("lab"), HostId::named("devbox")]);
    }

    #[test]
    fn a_disconnected_host_is_not_queried() {
        let configured = ["lab".to_string(), "devbox".to_string()];
        let order = query_order(&configured, |host| host.name() == "devbox");
        assert_eq!(order, [HostId::local(), HostId::named("devbox")]);
    }

    #[test]
    fn the_merge_is_pending_until_every_queried_host_answered() {
        let (order, a, b) = two_hosts();
        let mut answers = HashMap::new();
        assert!(merge(&order, &answers).pending, "nobody answered");
        answers.insert(HostId::local(), answer(&[], None, false));
        answers.insert(a.clone(), answer(&[], None, false));
        assert!(merge(&order, &answers).pending, "b has not answered");
        // Indexing progress alone is not an answer.
        answers.insert(b.clone(), HostSearch { progress: Some(30), ..HostSearch::default() });
        assert!(merge(&order, &answers).pending);
        answers.insert(b.clone(), answer(&[], None, true));
        assert!(!merge(&order, &answers).pending, "an unavailable host will not answer");
        answers.insert(b, answer(&[], None, false));
        assert!(!merge(&order, &answers).pending);
    }
}
