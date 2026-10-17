//! DXF file structure: header variables, tables, blocks and entity records.

use super::reader::Pair;
use anyhow::{Result, bail};
use std::collections::HashMap;

/// One record (entity, table entry, …): its type name and its group pairs
/// (excluding the leading `0 / TYPE` pair).
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub kind: String,
    pub pairs: Vec<Pair>,
    /// VERTEX records of a POLYLINE, ATTRIB records of an INSERT.
    pub children: Vec<Record>,
}

impl Record {
    pub fn get(&self, code: i32) -> Option<&Pair> {
        self.pairs.iter().find(|p| p.code == code)
    }
    pub fn f(&self, code: i32) -> Option<f64> {
        self.get(code).and_then(Pair::f)
    }
    pub fn f_or(&self, code: i32, d: f64) -> f64 {
        self.f(code).unwrap_or(d)
    }
    pub fn i(&self, code: i32) -> Option<i64> {
        self.get(code).and_then(Pair::i)
    }
    pub fn i_or(&self, code: i32, d: i64) -> i64 {
        self.i(code).unwrap_or(d)
    }
    pub fn s(&self, code: i32) -> Option<String> {
        self.get(code).map(|p| p.s().into_owned())
    }
    /// 3D point from codes `code`, `code+10`, `code+20`.
    pub fn pt3(&self, code: i32) -> Option<[f64; 3]> {
        let x = self.f(code)?;
        let y = self.f(code + 10).unwrap_or(0.0);
        let z = self.f(code + 20).unwrap_or(0.0);
        Some([x, y, z])
    }
    pub fn pt3_or0(&self, code: i32) -> [f64; 3] {
        self.pt3(code).unwrap_or([0.0; 3])
    }
    /// Extrusion direction (210/220/230), default +Z.
    pub fn normal(&self) -> [f64; 3] {
        match (self.f(210), self.f(220), self.f(230)) {
            (None, None, None) => [0.0, 0.0, 1.0],
            (x, y, z) => [x.unwrap_or(0.0), y.unwrap_or(0.0), z.unwrap_or(1.0)],
        }
    }
    pub fn layer(&self) -> String {
        self.s(8).unwrap_or_else(|| "0".into())
    }
}

#[derive(Debug, Clone)]
pub struct Block {
    pub name: String,
    pub base: [f64; 3],
    pub flags: i64,
    pub entities: Vec<Record>,
}

#[derive(Debug, Default)]
pub struct Drawing {
    pub header: HashMap<String, Vec<Pair>>,
    /// Table entries by table type (LAYER, LTYPE, …), in file order.
    pub tables: HashMap<String, Vec<Record>>,
    /// Blocks keyed by upper-case name.
    pub blocks: HashMap<String, Block>,
    pub entities: Vec<Record>,
}

impl Drawing {
    pub fn header_f(&self, var: &str) -> Option<f64> {
        self.header.get(var)?.first()?.f()
    }
    pub fn header_i(&self, var: &str) -> Option<i64> {
        self.header.get(var)?.first()?.i()
    }
    pub fn table(&self, name: &str) -> &[Record] {
        self.tables.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Split a pair stream into records at every code-0 pair.
fn split_records(pairs: &[Pair]) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::new();
    for p in pairs {
        if p.code == 0 {
            out.push(Record { kind: p.s().trim().to_ascii_uppercase(), pairs: Vec::new(), children: Vec::new() });
        } else if let Some(r) = out.last_mut() {
            r.pairs.push(p.clone());
        }
    }
    out
}

/// Attach VERTEX / ATTRIB records (up to SEQEND) to their POLYLINE / INSERT.
fn group_entities(records: Vec<Record>) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::new();
    let mut open = false; // last record collects children
    for r in records {
        match r.kind.as_str() {
            "VERTEX" | "ATTRIB" if open => {
                if let Some(last) = out.last_mut() {
                    last.children.push(r);
                }
            }
            "SEQEND" => open = false,
            "VERTEX" | "ATTRIB" => {} // orphan sub-entity: ignore
            _ => {
                open = matches!(r.kind.as_str(), "POLYLINE" | "INSERT");
                out.push(r);
            }
        }
    }
    out
}

pub fn parse_structure(pairs: &[Pair]) -> Result<Drawing> {
    let mut d = Drawing::default();
    let mut i = 0;
    let mut saw_section = false;
    while i < pairs.len() {
        if !pairs[i].is(0, "SECTION") {
            if pairs[i].is(0, "EOF") {
                break;
            }
            i += 1;
            continue;
        }
        saw_section = true;
        let name = pairs.get(i + 1).filter(|p| p.code == 2).map(|p| p.s().to_ascii_uppercase()).unwrap_or_default();
        let start = i + 2;
        let mut end = start;
        while end < pairs.len() && !pairs[end].is(0, "ENDSEC") && !pairs[end].is(0, "EOF") {
            end += 1;
        }
        let body = &pairs[start.min(end)..end];
        match name.as_str() {
            "HEADER" => parse_header(body, &mut d),
            "TABLES" => parse_tables(body, &mut d),
            "BLOCKS" => parse_blocks(body, &mut d),
            "ENTITIES" => d.entities.extend(group_entities(split_records(body))),
            _ => {}
        }
        i = end + 1;
    }
    if !saw_section {
        bail!("not a DXF file: no SECTION found");
    }
    Ok(d)
}

fn parse_header(body: &[Pair], d: &mut Drawing) {
    let mut cur: Option<String> = None;
    for p in body {
        if p.code == 9 {
            let name = p.s().to_ascii_uppercase();
            d.header.entry(name.clone()).or_default();
            cur = Some(name);
        } else if let Some(c) = &cur {
            d.header.entry(c.clone()).or_default().push(p.clone());
        }
    }
}

fn parse_tables(body: &[Pair], d: &mut Drawing) {
    let mut table: Option<String> = None;
    for r in split_records(body) {
        match r.kind.as_str() {
            "TABLE" => table = r.s(2).map(|s| s.to_ascii_uppercase()),
            "ENDTAB" => table = None,
            _ => {
                if let Some(t) = &table {
                    d.tables.entry(t.clone()).or_default().push(r);
                }
            }
        }
    }
}

fn parse_blocks(body: &[Pair], d: &mut Drawing) {
    let mut cur: Option<Block> = None;
    let mut ents: Vec<Record> = Vec::new();
    for r in split_records(body) {
        match r.kind.as_str() {
            "BLOCK" => {
                ents.clear();
                cur = Some(Block {
                    name: r.s(2).or_else(|| r.s(3)).unwrap_or_default(),
                    base: r.pt3_or0(10),
                    flags: r.i_or(70, 0),
                    entities: Vec::new(),
                });
            }
            "ENDBLK" => {
                if let Some(mut b) = cur.take() {
                    b.entities = group_entities(std::mem::take(&mut ents));
                    d.blocks.insert(b.name.to_ascii_uppercase(), b);
                }
            }
            _ => {
                if cur.is_some() {
                    ents.push(r);
                }
            }
        }
    }
    // Unterminated last block.
    if let Some(mut b) = cur.take() {
        b.entities = group_entities(ents);
        d.blocks.insert(b.name.to_ascii_uppercase(), b);
    }
}
