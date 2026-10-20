//! The graph view's data: a vault's notes and the links between them.
//! Built from decrypted notes in the browser; pure, so tested natively.

pub mod layout;

use std::collections::{BTreeSet, HashMap, VecDeque};

use coolpsonotes_core::markdown::{self, Link};
use coolpsonotes_core::paths;

/// One note in the graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    pub path: String,
    pub icon: Option<String>,
}

impl Node {
    pub fn title(&self) -> &str {
        paths::title(&self.path)
    }

    /// Top-level folder ("" for notes at the vault's root): their colour.
    pub fn folder(&self) -> &str {
        self.path.split_once('/').map(|(f, _)| f).unwrap_or("")
    }
}

/// Notes and the links between them, each pair once whichever way it links.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<(usize, usize)>,
}

/// A note as it goes into [`Graph::build`].
pub struct NoteLinks {
    pub id: String,
    pub path: String,
    pub icon: Option<String>,
    pub links: Vec<Link>,
}

impl Graph {
    /// From the notes and the vault's entries (`(id, path)`, attachments
    /// included, for resolving links). Links to attachments, to missing
    /// notes and to the note itself are left out.
    pub fn build(notes: Vec<NoteLinks>, entries: &[(String, String)]) -> Graph {
        let index: HashMap<&str, usize> = notes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let mut edges = BTreeSet::new();
        for (i, n) in notes.iter().enumerate() {
            for l in &n.links {
                if let Some(&j) = markdown::resolve(l, &n.path, entries).and_then(|t| index.get(t))
                    && j != i
                {
                    edges.insert((i.min(j), i.max(j)));
                }
            }
        }
        Graph {
            nodes: notes
                .into_iter()
                .map(|n| Node {
                    id: n.id,
                    path: n.path,
                    icon: n.icon,
                })
                .collect(),
            edges: edges.into_iter().collect(),
        }
    }

    /// How many notes each note is linked with.
    pub fn degrees(&self) -> Vec<usize> {
        let mut d = vec![0; self.nodes.len()];
        for &(a, b) in &self.edges {
            d[a] += 1;
            d[b] += 1;
        }
        d
    }

    pub fn neighbors(&self) -> Vec<Vec<usize>> {
        let mut n = vec![Vec::new(); self.nodes.len()];
        for &(a, b) in &self.edges {
            n[a].push(b);
            n[b].push(a);
        }
        n
    }

    /// The notes within `depth` links of `focus` (with the links among them).
    /// Empty if `focus` isn't in the graph.
    pub fn around(&self, focus: &str, depth: usize) -> Graph {
        let Some(start) = self.nodes.iter().position(|n| n.id == focus) else {
            return Graph::default();
        };
        let neighbors = self.neighbors();
        let mut dist = vec![usize::MAX; self.nodes.len()];
        dist[start] = 0;
        let mut queue = VecDeque::from([start]);
        while let Some(i) = queue.pop_front() {
            if dist[i] == depth {
                continue;
            }
            for &j in &neighbors[i] {
                if dist[j] == usize::MAX {
                    dist[j] = dist[i] + 1;
                    queue.push_back(j);
                }
            }
        }
        self.keep(|i| dist[i] != usize::MAX)
    }

    /// The notes whose title or path contains `query` (ignoring case), and
    /// without notes that link with nothing unless `orphans`.
    pub fn filtered(&self, query: &str, orphans: bool) -> Graph {
        let q = query.trim().to_lowercase();
        let degrees = self.degrees();
        self.keep(|i| {
            (q.is_empty() || self.nodes[i].path.to_lowercase().contains(&q))
                && (orphans || degrees[i] > 0)
        })
    }

    fn keep(&self, keep: impl Fn(usize) -> bool) -> Graph {
        let mut new_index = vec![None; self.nodes.len()];
        let mut nodes = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if keep(i) {
                new_index[i] = Some(nodes.len());
                nodes.push(n.clone());
            }
        }
        let edges = self
            .edges
            .iter()
            .filter_map(|&(a, b)| Some((new_index[a]?, new_index[b]?)))
            .collect();
        Graph { nodes, edges }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, path: &str, text: &str) -> NoteLinks {
        NoteLinks {
            id: id.into(),
            path: path.into(),
            icon: None,
            links: markdown::analyze(text).links,
        }
    }

    fn sample() -> Graph {
        let notes = vec![
            note("a", "A.md", "see [[B]] and [[B]] again, [[Missing]], [[A]]"),
            note(
                "b",
                "Travel/B.md",
                "back to [[A]], on to [[C]], ![[photo.png]]",
            ),
            note("c", "Travel/C.md", "nothing"),
            note("d", "D.md", "alone"),
        ];
        let entries: Vec<(String, String)> = [
            ("a", "A.md"),
            ("b", "Travel/B.md"),
            ("c", "Travel/C.md"),
            ("d", "D.md"),
            ("p", "photo.png"),
        ]
        .iter()
        .map(|(i, p)| (i.to_string(), p.to_string()))
        .collect();
        Graph::build(notes, &entries)
    }

    #[test]
    fn links_become_edges_once_and_only_between_notes() {
        let g = sample();
        // a–b (both ways, twice), b–c; not the missing note, the attachment
        // or a's link to itself.
        assert_eq!(g.edges, vec![(0, 1), (1, 2)]);
        assert_eq!(g.degrees(), vec![1, 2, 1, 0]);
        assert_eq!(g.nodes[1].title(), "B");
        assert_eq!((g.nodes[1].folder(), g.nodes[0].folder()), ("Travel", ""));
    }

    #[test]
    fn a_notes_neighbourhood() {
        let g = sample();
        let ids = |g: &Graph| g.nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
        let near = g.around("a", 1);
        assert_eq!(
            (ids(&near), near.edges.clone()),
            (vec!["a".into(), "b".into()], vec![(0, 1)])
        );
        assert_eq!(ids(&g.around("a", 2)), ["a", "b", "c"]);
        assert_eq!(ids(&g.around("d", 2)), ["d"]);
        assert!(g.around("nope", 1).nodes.is_empty());
    }

    #[test]
    fn filters_keep_the_links_among_what_is_left() {
        let g = sample();
        let travel = g.filtered("travel", true);
        assert_eq!(travel.nodes.len(), 2);
        assert_eq!(travel.edges, vec![(0, 1)]);
        let linked = g.filtered("", false);
        assert!(linked.nodes.iter().all(|n| n.id != "d"), "no orphans");
        assert_eq!(linked.edges.len(), 2);
    }
}
