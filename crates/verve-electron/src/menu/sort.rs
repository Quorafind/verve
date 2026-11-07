//! The order a menu template is put in: `before`, `after`, `beforeGroupContaining` and
//! `afterGroupContaining` placed, and the separators that end up first, last or doubled removed.
//!
//! A port of Electron 39.8.10's `lib/browser/api/menu-utils.ts` (`sortMenuItems`, as compiled
//! into `electron.exe`) and of `removeExtraSeparators` in its `menu.ts`, over the few fields
//! they read. Items are named by their position in the template; the answer is positions again,
//! so the caller moves its own objects.

/// The fields of a template item the order depends on.
#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub id: Option<String>,
    /// `type: 'separator'`.
    pub separator: bool,
    pub before: Option<Vec<String>>,
    pub after: Option<Vec<String>>,
    pub before_group: Option<Vec<String>>,
    pub after_group: Option<Vec<String>>,
}

impl Entry {
    /// A separator that says nothing of where it goes: one that only divides groups.
    fn is_plain_separator(&self) -> bool {
        self.separator
            && self.before.is_none()
            && self.after.is_none()
            && self.before_group.is_none()
            && self.after_group.is_none()
    }
}

/// An item of the sorted list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The template item at this position.
    Item(usize),
    /// A separator between groups: the template's own plain separator at this position, or a new one.
    Separator(Option<usize>),
}

type Group = Vec<usize>;
/// For each node, the nodes that must come before it.
type Edges = Vec<Vec<usize>>;

/// Sorts `entries`: groups are what lies between plain separators; `before`/`after` order the
/// items of a group (and join groups that name each other), `*GroupContaining` order the groups.
pub fn sort_entries(entries: &[Entry]) -> Vec<Slot> {
    let mut separators: Vec<usize> = (0..entries.len())
        .filter(|at| entries[*at].is_plain_separator())
        .collect();
    let mut groups = split(entries);
    while merge_one(entries, &mut groups) {}
    let groups: Vec<Group> = groups.iter().map(|g| sort_in_group(entries, g)).collect();
    let groups = sort_groups(entries, groups);
    let mut out = Vec::new();
    for (at, group) in groups.into_iter().enumerate() {
        if at > 0 && !group.is_empty() {
            let used = (!separators.is_empty()).then(|| separators.remove(0));
            out.push(Slot::Separator(used));
        }
        out.extend(group.into_iter().map(Slot::Item));
    }
    out
}

/// The items between plain separators, in order; no empty group at the end.
fn split(entries: &[Entry]) -> Vec<Group> {
    let mut groups: Vec<Group> = vec![Vec::new()];
    for (at, entry) in entries.iter().enumerate() {
        let last = groups.last_mut().expect("there is always a group");
        if !entry.is_plain_separator() {
            last.push(at);
        } else if !last.is_empty() {
            groups.push(Vec::new());
        }
    }
    if groups.last().is_some_and(Vec::is_empty) {
        groups.pop();
    }
    groups
}

/// The first group that holds an item with `id`, other than group `ignored`.
fn group_containing(
    entries: &[Entry],
    groups: &[Group],
    id: &str,
    ignored: usize,
) -> Option<usize> {
    groups.iter().enumerate().position(|(at, group)| {
        at != ignored
            && group
                .iter()
                .any(|item| entries[*item].id.as_deref() == Some(id))
    })
}

/// Joins one group into the group an item of it names with `before` or `after`; whether one was.
fn merge_one(entries: &[Entry], groups: &mut Vec<Group>) -> bool {
    for at in 0..groups.len() {
        let mut named = groups[at].iter().flat_map(|item| {
            let entry = &entries[*item];
            entry.before.iter().chain(entry.after.iter()).flatten()
        });
        let target = named.find_map(|id| group_containing(entries, groups, id, at));
        if let Some(target) = target {
            let moved = groups[at].clone();
            groups[target].extend(moved);
            groups.remove(at);
            return true;
        }
    }
    false
}

fn push_edge(edges: &mut Edges, from: usize, to: usize) {
    edges[from].push(to);
}

/// The nodes `0..edges.len()`, each after the nodes `edges` lists for it, depth first, and in order
/// otherwise.
fn sort_topologically(edges: &Edges) -> Vec<usize> {
    fn visit(node: usize, edges: &Edges, seen: &mut [bool], sorted: &mut Vec<usize>) {
        if std::mem::replace(&mut seen[node], true) {
            return;
        }
        for next in &edges[node] {
            visit(*next, edges, seen, sorted);
        }
        sorted.push(node);
    }
    let mut sorted = Vec::new();
    let mut seen = vec![false; edges.len()];
    for node in 0..edges.len() {
        visit(node, edges, &mut seen, &mut sorted);
    }
    sorted
}

/// The items of one group, placed by their `before` and `after`.
fn sort_in_group(entries: &[Entry], group: &Group) -> Group {
    // The position of the last item with each id, as the lookup of a map would give it.
    let position = |id: &String| {
        group
            .iter()
            .rposition(|item| entries[*item].id.as_ref() == Some(id))
    };
    let mut edges: Edges = vec![Vec::new(); group.len()];
    for (at, item) in group.iter().enumerate() {
        let entry = &entries[*item];
        for other in entry.before.iter().flatten().filter_map(position) {
            push_edge(&mut edges, other, at);
        }
        for other in entry.after.iter().flatten().filter_map(position) {
            push_edge(&mut edges, at, other);
        }
    }
    sort_topologically(&edges)
        .into_iter()
        .map(|at| group[at])
        .collect()
}

/// The groups placed by `beforeGroupContaining` and `afterGroupContaining`.
fn sort_groups(entries: &[Entry], groups: Vec<Group>) -> Vec<Group> {
    let mut edges: Edges = vec![Vec::new(); groups.len()];
    for at in 0..groups.len() {
        group_edge(entries, &groups, at, &mut edges);
    }
    sort_topologically(&edges)
        .into_iter()
        .map(|at| groups[at].clone())
        .collect()
}

/// The first edge the items of group `at` ask for; a group asks for at most one.
fn group_edge(entries: &[Entry], groups: &[Group], at: usize, edges: &mut Edges) {
    for item in &groups[at] {
        let entry = &entries[*item];
        for id in entry.before_group.iter().flatten() {
            if let Some(other) = group_containing(entries, groups, id, at) {
                return push_edge(edges, other, at);
            }
        }
        for id in entry.after_group.iter().flatten() {
            if let Some(other) = group_containing(entries, groups, id, at) {
                return push_edge(edges, at, other);
            }
        }
    }
}

/// Which of `items` — `(is a separator, is explicitly invisible)` — `removeExtraSeparators`
/// keeps: an invisible item always stays, a separator goes after another one, and at the start
/// and at the end.
pub fn keep_after_extra_separators(items: &[(bool, bool)]) -> Vec<bool> {
    let first: Vec<usize> = (0..items.len())
        .filter(|at| {
            let (separator, invisible) = items[*at];
            invisible || !separator || *at == 0 || !items[*at - 1].0
        })
        .collect();
    let mut keep = vec![false; items.len()];
    for (at, item) in first.iter().enumerate() {
        let (separator, invisible) = items[*item];
        keep[*item] = invisible || !separator || (at != 0 && at != first.len() - 1);
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Entry {
        Entry {
            id: Some(id.to_owned()),
            ..Entry::default()
        }
    }

    fn separator() -> Entry {
        Entry {
            separator: true,
            ..Entry::default()
        }
    }

    fn ids(entries: &[Entry], slots: &[Slot]) -> Vec<String> {
        slots
            .iter()
            .map(|slot| match slot {
                Slot::Item(at) => entries[*at].id.clone().unwrap_or_else(|| "?".into()),
                Slot::Separator(_) => "-".into(),
            })
            .collect()
    }

    fn list(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn items_keep_their_order_and_groups_are_divided_by_one_separator() {
        let entries = [
            separator(),
            item("a"),
            separator(),
            separator(),
            item("b"),
            separator(),
        ];
        let slots = sort_entries(&entries);
        assert_eq!(ids(&entries, &slots), list(&["a", "-", "b"]));
        // The separator between is the first the template had.
        assert_eq!(slots[1], Slot::Separator(Some(0)));
    }

    #[test]
    fn the_separators_between_groups_are_the_templates_own_in_order() {
        let mut b = item("b");
        b.before_group = Some(vec!["a".into()]);
        let entries = [item("a"), separator(), b, separator(), item("c")];
        let slots = sort_entries(&entries);
        assert_eq!(slots[1], Slot::Separator(Some(1)));
        assert_eq!(slots[3], Slot::Separator(Some(3)));
    }

    #[test]
    fn a_group_that_merges_leaves_a_separator_of_the_template_unused() {
        let mut b = item("b");
        b.after = Some(vec!["a".into()]);
        let entries = [item("a"), separator(), b, separator(), item("c")];
        assert_eq!(
            sort_entries(&entries),
            [
                Slot::Item(0),
                Slot::Item(2),
                Slot::Separator(Some(1)),
                Slot::Item(4)
            ]
        );
    }

    #[test]
    fn before_and_after_place_an_item_beside_the_one_they_name() {
        let mut two = item("2");
        two.before = Some(vec!["1".into()]);
        let mut three = item("3");
        three.after = Some(vec!["1".into()]);
        let entries = [item("1"), two, three];
        assert_eq!(
            ids(&entries, &sort_entries(&entries)),
            list(&["2", "1", "3"])
        );
    }

    #[test]
    fn group_containing_places_a_whole_group() {
        let mut b = item("b");
        b.before_group = Some(vec!["a".into()]);
        let mut c = item("c");
        c.after_group = Some(vec!["a".into()]);
        let entries = [item("a"), separator(), b, separator(), c];
        assert_eq!(
            ids(&entries, &sort_entries(&entries)),
            list(&["b", "-", "a", "-", "c"])
        );
    }

    #[test]
    fn an_item_that_names_one_in_another_group_pulls_its_group_in() {
        let mut b = item("b");
        b.after = Some(vec!["a".into()]);
        let entries = [item("a"), separator(), b, separator(), item("c")];
        assert_eq!(
            ids(&entries, &sort_entries(&entries)),
            list(&["a", "b", "-", "c"])
        );
    }

    #[test]
    fn a_cycle_is_cut_where_it_closes() {
        let mut a = item("a");
        a.after = Some(vec!["b".into()]);
        let mut b = item("b");
        b.after = Some(vec!["a".into()]);
        let entries = [a, b];
        assert_eq!(ids(&entries, &sort_entries(&entries)), list(&["b", "a"]));
    }

    #[test]
    fn a_separator_that_says_where_it_goes_is_an_item() {
        let mut placed = separator();
        placed.after = Some(vec!["a".into()]);
        let entries = [item("a"), placed];
        assert_eq!(sort_entries(&entries), [Slot::Item(0), Slot::Item(1)]);
    }

    #[test]
    fn extra_separators_go_at_the_start_the_end_and_after_another() {
        let sep = (true, false);
        let item = (false, false);
        let keep = keep_after_extra_separators(&[sep, item, sep, sep, item, sep]);
        assert_eq!(keep, [false, true, true, false, true, false]);
        let hidden = (true, true);
        let keep = keep_after_extra_separators(&[hidden, item, hidden]);
        assert_eq!(keep, [true, true, true]);
    }
}
