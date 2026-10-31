//! Track D (Traverse): exact shortest-path distances for 2,000 (s, t) queries on a
//! 5M-edge directed road network, or -1 when t is unreachable.
//!
//! How it is fast:
//! 1. Both files are mmap'd (pre-warmed page cache: no memory charge, no copy).
//! 2. Every thread parses its own slice of the edge list. Out-degrees are counted
//!    with atomic increments, then a prefix sum and a second atomic cursor scatter
//!    the edges into one CSR array (offsets + packed (head, weight) pairs), so the
//!    graph is built in parallel with no per-node allocation.
//! 3. Queries run in parallel, one Dijkstra per query, each thread reusing its own
//!    distance array and resetting only the nodes it touched. Nodes are first
//!    renumbered in BFS order so that a search's nodes sit close together in memory.
//! 4. A target with no in-edges is answered -1 at once: the reference settles the
//!    whole graph to find that out.

mod mmap;

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::thread;

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    if let Err(message) = run(&data_dir, &out_dir) {
        eprintln!("track-d: {message}");
        std::process::exit(1);
    }
}

struct Graph {
    /// Edges of node u are `edges[offsets[u]..offsets[u + 1]]`.
    offsets: Vec<u32>,
    /// `(head, weight)` pairs.
    edges: Vec<(u32, u32)>,
    /// 1 if the node has at least one in-edge.
    has_in: Vec<AtomicU8>,
}

fn run(data_dir: &str, out_dir: &str) -> Result<(), String> {
    let open = |name: &str| -> Result<&'static [u8], String> {
        let path = format!("{data_dir}/{name}");
        let file = File::open(&path).map_err(|e| format!("open {path}: {e}"))?;
        mmap::map(&file).map_err(|e| format!("mmap {path}: {e}"))
    };
    let graph_txt = open("graph.txt")?;
    let queries_txt = open("queries.txt")?;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());

    let (graph, new_id) = relabel(build_graph(graph_txt, threads)?, threads);
    let n = graph.offsets.len() - 1;

    let mut queries = Vec::new();
    let mut i = 0;
    while i < queries_txt.len() {
        let (s, j) = parse_u32(queries_txt, i);
        let (t, j) = parse_u32(queries_txt, j + 1);
        if s as usize >= n || t as usize >= n {
            return Err(format!("query {s} {t} names a node outside 0..{n}"));
        }
        queries.push((new_id[s as usize], new_id[t as usize]));
        i = j + 1;
    }

    // Answer the queries in parallel; threads take the next unanswered one.
    let answers: Vec<AtomicI64Cell> = (0..queries.len()).map(|_| AtomicI64Cell::default()).collect();
    let next = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..threads.min(queries.len().max(1)) {
            s.spawn(|| {
                let mut search = Search::new(n);
                loop {
                    let q = next.fetch_add(1, Relaxed);
                    let Some(&(source, target)) = queries.get(q) else { break };
                    answers[q].set(search.distance(&graph, source, target));
                }
            });
        }
    });

    let mut out = Vec::with_capacity(queries.len() * 12);
    for answer in &answers {
        match answer.get() {
            -1 => out.extend_from_slice(b"-1"),
            d => push_decimal(&mut out, d as u64),
        }
        out.push(b'\n');
    }
    let output_path = format!("{out_dir}/distances.txt");
    File::create(&output_path)
        .and_then(|mut f| f.write_all(&out))
        .map_err(|e| format!("write {output_path}: {e}"))
}

/// A per-query result slot written by exactly one thread.
#[derive(Default)]
struct AtomicI64Cell(std::sync::atomic::AtomicI64);

impl AtomicI64Cell {
    fn set(&self, value: i64) {
        self.0.store(value, Relaxed)
    }
    fn get(&self) -> i64 {
        self.0.load(Relaxed)
    }
}

fn build_graph(text: &[u8], threads: usize) -> Result<Graph, String> {
    let (n, j) = parse_u32(text, 0);
    let (m, j) = parse_u32(text, j + 1);
    let (n, m) = (n as usize, m as usize);
    let body = &text[j + 1..];

    // Pass 1, parallel: parse each slice, count out-degrees, mark heads.
    let degree: Vec<AtomicU32> = (0..=n).map(|_| AtomicU32::new(0)).collect();
    let has_in: Vec<AtomicU8> = (0..n).map(|_| AtomicU8::new(0)).collect();
    let parsed: Vec<Vec<[u32; 3]>> = thread::scope(|s| {
        let handles: Vec<_> = split_at_lines(body, threads)
            .into_iter()
            .map(|chunk| {
                let (degree, has_in) = (&degree, &has_in);
                s.spawn(move || -> Result<Vec<[u32; 3]>, String> {
                    let mut edges = Vec::with_capacity(chunk.len() / 18 + 16);
                    let mut i = 0;
                    while i < chunk.len() {
                        let (u, j) = parse_u32(chunk, i);
                        let (v, j) = parse_u32(chunk, j + 1);
                        let (w, j) = parse_u32(chunk, j + 1);
                        if u as usize >= n || v as usize >= n {
                            return Err(format!("edge {u} {v} names a node outside 0..{n}"));
                        }
                        degree[u as usize].fetch_add(1, Relaxed);
                        has_in[v as usize].store(1, Relaxed);
                        edges.push([u, v, w]);
                        i = j + 1;
                    }
                    Ok(edges)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, _>>()
    })?;
    let total: usize = parsed.iter().map(Vec::len).sum();
    if total != m {
        return Err(format!("header says {m} edges, file has {total}"));
    }

    // Exclusive prefix sum: offsets[u] is where u's edges start. `degree` is reused
    // as each node's write cursor.
    let mut offsets = Vec::with_capacity(n + 1);
    let mut sum = 0u32;
    for d in &degree {
        offsets.push(sum);
        sum += d.swap(sum, Relaxed);
    }

    // Pass 2, parallel: scatter every edge into its node's range. The order inside
    // one node's range depends on thread timing, which cannot change a distance.
    let mut edges = vec![(0u32, 0u32); m];
    let target = SharedEdges(edges.as_mut_ptr());
    thread::scope(|s| {
        for part in &parsed {
            let (degree, target) = (&degree, &target);
            s.spawn(move || {
                for &[u, v, w] in part {
                    let slot = degree[u as usize].fetch_add(1, Relaxed);
                    // SAFETY: each fetch_add hands out a distinct slot inside u's range,
                    // which lies inside `edges`, so writes never overlap or overflow.
                    unsafe { *target.0.add(slot as usize) = (v, w) };
                }
            });
        }
    });
    Ok(Graph { offsets, edges, has_in })
}

/// Renumbers the nodes in breadth-first order. Node ids in the input are a random
/// relabelling, so a Dijkstra ball of 10,000 nodes touched 10,000 random cache
/// lines in each of `offsets`, `edges` and `dist`. In BFS order, nodes a few hops
/// apart get nearby ids, and the same ball reads a few hundred compact runs.
/// Returns the renumbered graph and `new_id[old id]`.
fn relabel(graph: Graph, threads: usize) -> (Graph, Vec<u32>) {
    let n = graph.offsets.len() - 1;
    let edges_of = |u: u32| &graph.edges[graph.offsets[u as usize] as usize..graph.offsets[u as usize + 1] as usize];
    let mut new_id = vec![u32::MAX; n];
    let mut order: Vec<u32> = Vec::with_capacity(n);
    for root in 0..n as u32 {
        if new_id[root as usize] != u32::MAX {
            continue;
        }
        new_id[root as usize] = order.len() as u32;
        order.push(root);
        let mut head = order.len() - 1;
        while head < order.len() {
            // The queue says which nodes come next: prefetch their offsets and edges
            // so the misses overlap instead of stalling one after another.
            if let Some(&ahead) = order.get(head + 16) {
                prefetch(graph.offsets.get(ahead as usize));
            }
            if let Some(&ahead) = order.get(head + 8) {
                prefetch(graph.edges.get(graph.offsets[ahead as usize] as usize));
            }
            let u = order[head];
            head += 1;
            for &(v, _) in edges_of(u) {
                if new_id[v as usize] == u32::MAX {
                    new_id[v as usize] = order.len() as u32;
                    order.push(v);
                }
            }
        }
    }

    // Copy every node's edges to its new position, heads renamed, in parallel.
    let mut offsets = Vec::with_capacity(n + 1);
    let mut sum = 0u32;
    for &u in &order {
        offsets.push(sum);
        sum += edges_of(u).len() as u32;
    }
    offsets.push(sum);
    let mut edges = vec![(0u32, 0u32); graph.edges.len()];
    let has_in: Vec<AtomicU8> = order.iter().map(|&u| AtomicU8::new(graph.has_in[u as usize].load(Relaxed))).collect();
    thread::scope(|s| {
        let mut rest: &mut [(u32, u32)] = &mut edges;
        let per = n.div_ceil(threads);
        for x0 in (0..n).step_by(per.max(1)) {
            let x1 = (x0 + per).min(n);
            let (mine, tail) = rest.split_at_mut((offsets[x1] - offsets[x0]) as usize);
            rest = tail;
            let (order, new_id, edges_of) = (&order, &new_id, &edges_of);
            s.spawn(move || {
                let mut k = 0;
                for &u in &order[x0..x1] {
                    for &(v, w) in edges_of(u) {
                        mine[k] = (new_id[v as usize], w);
                        k += 1;
                    }
                }
            });
        }
    });
    (Graph { offsets, edges, has_in }, new_id)
}

fn prefetch<T>(item: Option<&T>) {
    #[cfg(target_arch = "x86_64")]
    if let Some(item) = item {
        // SAFETY: prefetching is only a hint and never faults.
        unsafe { std::arch::x86_64::_mm_prefetch(item as *const T as *const i8, std::arch::x86_64::_MM_HINT_T0) };
    }
}

struct SharedEdges(*mut (u32, u32));
// SAFETY: only used for the disjoint scatter writes above.
unsafe impl Sync for SharedEdges {}

/// One thread's reusable Dijkstra state.
struct Search {
    /// Distance + 1, or 0 for "not reached". Zero-filled memory comes from calloc,
    /// so pages of nodes this thread never reaches are never touched or charged.
    dist: Vec<u64>,
    touched: Vec<u32>,
    heap: RadixHeap,
}

impl Search {
    fn new(n: usize) -> Self {
        Search { dist: vec![0; n], touched: Vec::new(), heap: RadixHeap::new() }
    }

    fn distance(&mut self, graph: &Graph, source: u32, target: u32) -> i64 {
        if source == target {
            return 0;
        }
        if graph.has_in[target as usize].load(Relaxed) == 0 {
            return -1; // nothing points at the target, so no path can end there
        }
        for &u in &self.touched {
            self.dist[u as usize] = 0;
        }
        self.touched.clear();
        self.heap.clear();

        self.dist[source as usize] = 1;
        self.touched.push(source);
        self.heap.push(0, source);
        while let Some((d, u)) = self.heap.pop() {
            if d + 1 > self.dist[u as usize] {
                continue; // stale entry
            }
            if u == target {
                return d as i64;
            }
            let range = graph.offsets[u as usize] as usize..graph.offsets[u as usize + 1] as usize;
            for &(v, w) in &graph.edges[range] {
                let candidate = d + w as u64;
                let slot = &mut self.dist[v as usize];
                if *slot == 0 || candidate + 1 < *slot {
                    if *slot == 0 {
                        self.touched.push(v);
                    }
                    *slot = candidate + 1;
                    self.heap.push(candidate, v);
                }
            }
        }
        -1
    }
}

/// A radix heap: a monotone priority queue for integer keys. Dijkstra only ever
/// pushes keys >= the last key popped, so an entry can live in bucket
/// `bit length of (key XOR last)`: bucket 0 holds keys equal to `last`, and every
/// key in bucket i shares `last`'s bits above bit i-1. Popping refills bucket 0 by
/// redistributing the lowest non-empty bucket once its minimum becomes `last`.
/// Each entry moves down at most 64 times in total, so push and pop are O(1)
/// amortized, on short contiguous vectors instead of a 15-level binary heap.
struct RadixHeap {
    last: u64,
    len: usize,
    buckets: [Vec<(u64, u32)>; 65],
}

impl RadixHeap {
    fn new() -> Self {
        RadixHeap { last: 0, len: 0, buckets: std::array::from_fn(|_| Vec::new()) }
    }

    fn clear(&mut self) {
        self.buckets.iter_mut().for_each(Vec::clear);
        self.last = 0;
        self.len = 0;
    }

    fn bucket(&self, key: u64) -> usize {
        (64 - (key ^ self.last).leading_zeros()) as usize
    }

    fn push(&mut self, key: u64, node: u32) {
        debug_assert!(key >= self.last);
        let b = self.bucket(key);
        self.buckets[b].push((key, node));
        self.len += 1;
    }

    fn pop(&mut self) -> Option<(u64, u32)> {
        if self.len == 0 {
            return None;
        }
        if self.buckets[0].is_empty() {
            let b = (1..65).find(|&b| !self.buckets[b].is_empty()).unwrap();
            let moving = std::mem::take(&mut self.buckets[b]);
            self.last = moving.iter().map(|&(key, _)| key).min().unwrap();
            for &(key, node) in &moving {
                let to = self.bucket(key);
                self.buckets[to].push((key, node));
            }
            self.buckets[b] = moving; // keep its capacity for later pushes
            self.buckets[b].clear();
        }
        self.len -= 1;
        self.buckets[0].pop()
    }
}

/// Parses the unsigned decimal at `text[i..]`; returns it and the index of the byte after it.
fn parse_u32(text: &[u8], mut i: usize) -> (u32, usize) {
    let mut value = 0u32;
    while let Some(&b) = text.get(i).filter(|b| b.is_ascii_digit()) {
        value = value * 10 + (b - b'0') as u32;
        i += 1;
    }
    (value, i)
}

fn push_decimal(out: &mut Vec<u8>, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}

/// Splits `data` into about `parts` slices, each ending just after a newline.
fn split_at_lines(data: &[u8], parts: usize) -> Vec<&[u8]> {
    let mut chunks = Vec::with_capacity(parts);
    let mut start = 0;
    for p in 1..=parts {
        let mut end = (data.len() * p / parts).max(start);
        while end < data.len() && (end == 0 || data[end - 1] != b'\n') {
            end += 1;
        }
        if end > start {
            chunks.push(&data[start..end]);
        }
        start = end;
    }
    chunks
}
