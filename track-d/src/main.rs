//! Track D (Traverse): exact shortest-path distances for 2,000 (s, t) queries on a
//! 5M-edge directed road network, or -1 when t is unreachable.
//!
//! How it is fast:
//! 1. Both files are mmap'd (pre-warmed page cache: no memory charge, no copy).
//! 2. Every thread parses its own slice of the edge list. Out-degrees are counted
//!    with atomic increments, then a prefix sum and a second atomic cursor scatter
//!    the edges into one CSR array (offsets + packed (head, weight) pairs), so the
//!    graph is built in parallel with no per-node allocation.
//! 3. Nodes are renumbered so that a search's nodes sit close together in memory:
//!    each thread grows a breadth-first region from its own seed (see `relabel`).
//! 4. Queries run in parallel, one Dijkstra per query with a radix heap and a
//!    paged distance table that stays in cache (see `Search`).
//! 5. A target with no in-edges is answered -1 at once: the reference settles the
//!    whole graph to find that out.

mod mmap;

use std::fs::File;
use std::io::Write;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::Relaxed};
use std::thread;

const UNCLAIMED: u8 = u8::MAX;

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

/// Source-node buckets for building the CSR: few enough that each parsing thread's
/// 128 append cursors stay in cache, enough that one bucket's nodes (~14K) and
/// their counts fit in L1/L2 while the bucket is laid out.
const BUCKET_BITS: u32 = 7;

/// Builds the CSR without a random write per edge. Pass 1 parses every slice in
/// parallel and appends each edge to the thread's bucket for its source node, a
/// sequential write. Pass 2 lays out one bucket at a time: its edges cover a
/// small range of source nodes, so counting their degrees and placing them is a
/// counting sort that stays in cache, written to a disjoint part of the CSR.
fn build_graph(text: &[u8], threads: usize) -> Result<Graph, String> {
    let (n, j) = parse_u32(text, 0);
    let (m, j) = parse_u32(text, j + 1);
    let (n, m) = (n as usize, m as usize);
    let body = &text[j + 1..];
    let shift = (usize::BITS - n.max(1).leading_zeros()).saturating_sub(BUCKET_BITS);
    let buckets = (n >> shift) + 1;

    // Pass 1, parallel: parse each slice into source-node buckets; mark heads.
    let has_in: Vec<AtomicU8> = (0..n).map(|_| AtomicU8::new(0)).collect();
    let parts: Vec<Vec<Vec<(u32, u32, u32)>>> = thread::scope(|s| {
        let handles: Vec<_> = split_at_lines(body, threads)
            .into_iter()
            .map(|chunk| {
                let has_in = &has_in;
                s.spawn(move || -> Result<Vec<Vec<(u32, u32, u32)>>, String> {
                    let mut out = vec![Vec::with_capacity(chunk.len() / 18 / buckets + 16); buckets];
                    let mut i = 0;
                    while i < chunk.len() {
                        let (u, j) = parse_u32(chunk, i);
                        let (v, j) = parse_u32(chunk, j + 1);
                        let (w, j) = parse_u32(chunk, j + 1);
                        if u as usize >= n || v as usize >= n {
                            return Err(format!("edge {u} {v} names a node outside 0..{n}"));
                        }
                        has_in[v as usize].store(1, Relaxed);
                        out[(u >> shift) as usize].push((u, v, w));
                        i = j + 1;
                    }
                    Ok(out)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<Result<_, _>>()
    })?;
    let total: usize = parts.iter().flatten().map(Vec::len).sum();
    if total != m {
        return Err(format!("header says {m} edges, file has {total}"));
    }

    // Where each bucket's edges start in the CSR.
    let mut bucket_start = vec![0usize; buckets + 1];
    for b in 0..buckets {
        bucket_start[b + 1] = bucket_start[b] + parts.iter().map(|part| part[b].len()).sum::<usize>();
    }

    // Pass 2, parallel: lay out bucket after bucket; each writes only its own
    // nodes' offsets and its own range of edges. Edges keep file order per node.
    let mut offsets = vec![0u32; n + 1];
    let mut edges = vec![(0u32, 0u32); m];
    let (offsets_out, edges_out) = (Shared(offsets.as_mut_ptr()), Shared(edges.as_mut_ptr()));
    let next = AtomicUsize::new(0);
    thread::scope(|s| {
        for _ in 0..threads {
            let (parts, bucket_start, next, offsets_out, edges_out) = (&parts, &bucket_start, &next, &offsets_out, &edges_out);
            s.spawn(move || {
                let mut cursor = Vec::new();
                loop {
                    let b = next.fetch_add(1, Relaxed);
                    if b >= buckets {
                        break;
                    }
                    let (lo, hi) = (b << shift, ((b + 1) << shift).min(n));
                    cursor.clear();
                    cursor.resize(hi - lo + 1, 0u32);
                    for &(u, _, _) in parts.iter().flat_map(|part| &part[b]) {
                        cursor[u as usize - lo + 1] += 1;
                    }
                    let mut at = bucket_start[b] as u32;
                    for (k, slot) in cursor.iter_mut().enumerate() {
                        at += *slot;
                        *slot = at;
                        if lo + k < hi {
                            // SAFETY: offsets[lo..hi] belong to this bucket alone.
                            unsafe { *offsets_out.0.add(lo + k) = at };
                        }
                    }
                    for &(u, v, w) in parts.iter().flat_map(|part| &part[b]) {
                        let slot = &mut cursor[u as usize - lo];
                        // SAFETY: the counting sort above gives every edge of this
                        // bucket a distinct slot inside its own range of `edges`.
                        unsafe { *edges_out.0.add(*slot as usize) = (v, w) };
                        *slot += 1;
                    }
                }
            });
        }
    });
    offsets[n] = m as u32;
    Ok(Graph { offsets, edges, has_in })
}

/// Renumbers the nodes so that nodes close in the graph get close ids. Input ids
/// are a random relabelling, so a Dijkstra ball of 10,000 nodes touched 10,000
/// random cache lines in each of `offsets`, `edges` and `dist`. Each thread runs a
/// breadth-first search from its own seed, claiming every node it reaches first
/// with a compare-and-swap, so the threads carve the graph into compact regions;
/// new ids go region by region, in BFS order within each, and the same ball then
/// reads a few hundred compact runs. Nodes no region reached (islands, nodes
/// without in-edges) come last. Returns the renumbered graph and `new_id[old id]`.
fn relabel(graph: Graph, threads: usize) -> (Graph, Vec<u32>) {
    let n = graph.offsets.len() - 1;
    let edges_of = |u: u32| &graph.edges[graph.offsets[u as usize] as usize..graph.offsets[u as usize + 1] as usize];
    let owner: Vec<AtomicU8> = (0..n).map(|_| AtomicU8::new(UNCLAIMED)).collect();
    let claim = |v: u32, by: usize| {
        owner[v as usize].load(Relaxed) == UNCLAIMED
            && owner[v as usize].compare_exchange(UNCLAIMED, by as u8, Relaxed, Relaxed).is_ok()
    };
    let grow = |queue: &mut Vec<u32>, by: usize| {
        let mut head = queue.len() - 1;
        while head < queue.len() {
            // The queue says which nodes come next: prefetch their offsets and edges
            // so the cache misses overlap instead of stalling one after another.
            if let Some(&ahead) = queue.get(head + 16) {
                prefetch(graph.offsets.get(ahead as usize));
            }
            if let Some(&ahead) = queue.get(head + 8) {
                prefetch(graph.edges.get(graph.offsets[ahead as usize] as usize));
            }
            let u = queue[head];
            head += 1;
            for &(v, _) in edges_of(u) {
                if claim(v, by) {
                    queue.push(v);
                }
            }
        }
    };
    let regions = threads.min(UNCLAIMED as usize);
    let mut order: Vec<u32> = thread::scope(|s| {
        let handles: Vec<_> = (0..regions)
            .map(|t| {
                let (claim, grow) = (&claim, &grow);
                s.spawn(move || {
                    let seed = (n * t / regions) as u32;
                    let mut queue = Vec::new();
                    if n > 0 && claim(seed, t) {
                        queue.push(seed);
                        grow(&mut queue, t);
                    }
                    queue
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    for root in 0..n as u32 {
        if claim(root, 0) {
            order.push(root);
            grow(&mut order, 0);
        }
    }
    // Renumber in parallel: `order` holds every node exactly once (each was
    // claimed by one CAS), so the writes to `new_id` are disjoint, and so are the
    // degree slots of each chunk. Each step is a random access per node; done
    // serially these loops were most of the relabel time.
    let mut new_id = vec![0u32; n];
    let mut offsets = vec![0u32; n + 1];
    let has_in: Vec<AtomicU8> = (0..n).map(|_| AtomicU8::new(0)).collect();
    let per = n.div_ceil(threads).max(1);
    let (ids, degrees) = (Shared(new_id.as_mut_ptr()), Shared(offsets.as_mut_ptr()));
    thread::scope(|s| {
        let (ids, degrees, has_in, edges_of) = (&ids, &degrees, &has_in, &edges_of);
        for (c, chunk) in order.chunks(per).enumerate() {
            let has_in_old = &graph.has_in;
            s.spawn(move || {
                for (k, &u) in chunk.iter().enumerate() {
                    let x = c * per + k;
                    // SAFETY: see above; x + 1 <= n, and u < n.
                    unsafe {
                        *ids.0.add(u as usize) = x as u32;
                        *degrees.0.add(x + 1) = edges_of(u).len() as u32;
                    }
                    has_in[x].store(has_in_old[u as usize].load(Relaxed), Relaxed);
                }
            });
        }
    });
    for x in 0..n {
        offsets[x + 1] += offsets[x];
    }

    // Copy every node's edges to its new position, heads renamed, in parallel.
    let mut edges = vec![(0u32, 0u32); graph.edges.len()];
    thread::scope(|s| {
        let mut rest: &mut [(u32, u32)] = &mut edges;
        for x0 in (0..n).step_by(per) {
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

struct Shared<T>(*mut T);
// SAFETY: only used for the disjoint writes in build_graph and relabel.
unsafe impl<T> Sync for Shared<T> {}

/// One thread's reusable Dijkstra state.
struct Search {
    /// Tentative distances, as distance + 1 in 32 bits (0 = not reached), kept in
    /// pages of PAGE nodes handed out on first touch. A search reaches thousands
    /// of nodes that the locality-friendly numbering keeps in a few dozen pages, so
    /// its working set stays in L2 instead of spreading over one entry per node,
    /// and a thread's pool is a few MB instead of 7. A distance that does not fit
    /// (a path through one of the weights near 2^32) is stored as BIG with its
    /// exact value in `big`.
    page_of: Vec<u16>, // node >> PAGE_BITS -> pool page + 1, or 0
    pool: Vec<u32>,
    used: Vec<u32>, // the node pages in use by this search
    big: HashMap<u32, u64>,
    heap: RadixHeap,
}

const BIG: u32 = u32::MAX;
const PAGE_BITS: u32 = 12;
const PAGE: usize = 1 << PAGE_BITS;

impl Search {
    fn new(n: usize) -> Self {
        Search { page_of: vec![0; n.div_ceil(PAGE)], pool: Vec::new(), used: Vec::new(), big: HashMap::new(), heap: RadixHeap::new() }
    }

    // get and set run once per edge relaxation: always inlined, with the rare
    // over-2^32 path kept out of line.
    #[inline(always)]
    fn get(&self, node: u32) -> u64 {
        let page = self.page_of[(node >> PAGE_BITS) as usize];
        if page == 0 {
            return u64::MAX;
        }
        match self.pool[(page as usize - 1) * PAGE + (node as usize & (PAGE - 1))] {
            0 => u64::MAX,
            BIG => self.get_big(node),
            stored => stored as u64 - 1,
        }
    }

    #[cold]
    #[inline(never)]
    fn get_big(&self, node: u32) -> u64 {
        self.big[&node]
    }

    #[cold]
    #[inline(never)]
    fn set_big(&mut self, node: u32, d: u64) {
        self.big.insert(node, d);
    }

    #[inline(always)]
    fn set(&mut self, node: u32, d: u64) {
        let mut page = self.page_of[(node >> PAGE_BITS) as usize];
        if page == 0 {
            self.used.push(node >> PAGE_BITS);
            page = self.used.len() as u16;
            self.page_of[(node >> PAGE_BITS) as usize] = page;
            if self.pool.len() < self.used.len() * PAGE {
                self.pool.resize(self.used.len() * PAGE, 0);
            }
        }
        let slot = &mut self.pool[(page as usize - 1) * PAGE + (node as usize & (PAGE - 1))];
        if d + 1 < BIG as u64 {
            *slot = d as u32 + 1;
        } else {
            *slot = BIG;
            self.set_big(node, d);
        }
    }

    /// Returns every page used by the last search, zeroed.
    fn clear(&mut self) {
        for &page in &self.used {
            self.page_of[page as usize] = 0;
        }
        self.pool[..self.used.len() * PAGE].fill(0);
        self.used.clear();
        self.big.clear();
        self.heap.clear();
    }

    fn distance(&mut self, graph: &Graph, source: u32, target: u32) -> i64 {
        if source == target {
            return 0;
        }
        let leaves = graph.offsets[source as usize + 1] > graph.offsets[source as usize];
        if graph.has_in[target as usize].load(Relaxed) == 0 || !leaves {
            return -1; // no path can end at the target, or leave the source
        }
        self.clear();
        self.set(source, 0);
        self.heap.push(0, source);
        while let Some((d, u)) = self.heap.pop() {
            if d > self.get(u) {
                continue; // stale entry
            }
            if u == target {
                return d as i64;
            }
            let range = graph.offsets[u as usize] as usize..graph.offsets[u as usize + 1] as usize;
            for &(v, w) in &graph.edges[range] {
                let candidate = d + w as u64;
                if candidate < self.get(v) {
                    self.set(v, candidate);
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
