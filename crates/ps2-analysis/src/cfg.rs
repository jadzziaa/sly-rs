//! Control-flow graphs: a function's basic blocks and the edges between
//! them. A block that ends in a branch or jump includes the delay slot.

use std::collections::BTreeSet;

use ps2_isa::ee::{Flow, Insn, Op};

use crate::{Function, Image, JumpTable};

/// Why control goes from one block to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// Falls through to the next block (also: a branch not taken).
    Fallthrough,
    /// A branch taken.
    Taken,
    /// A `j` within the function.
    Jump,
    /// A `switch` case through a jump table.
    Case,
}

/// An edge to a block of the same function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Edge {
    /// The target block's start.
    pub to: u32,
    /// Why.
    pub kind: EdgeKind,
}

/// A basic block: `[start, end)`, entered only at `start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// First instruction.
    pub start: u32,
    /// First address after the block.
    pub end: u32,
    /// Edges to blocks of the same function.
    pub edges: Vec<Edge>,
    /// Whether control can leave the function here (return, tail call,
    /// indirect jump, `eret`).
    pub exits: bool,
}

/// A function's control-flow graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cfg {
    /// Blocks by address; the first is the entry.
    pub blocks: Vec<Block>,
    /// Branch targets that land on a delay slot (blocks were split there;
    /// code generation must duplicate the slot).
    pub targets_in_delay_slots: Vec<u32>,
}

impl Cfg {
    /// Builds the graph of `function`; `tables` are the discovery's jump
    /// tables.
    #[must_use]
    pub fn build(image: &Image, function: &Function, tables: &[JumpTable]) -> Self {
        let range = function.start..function.end;
        let insn = |address: u32| Insn::decode(image.word(address).unwrap_or(0));
        let table = |site: u32| tables.iter().find(|t| t.site == site);

        let mut leaders = BTreeSet::from([function.start]);
        let mut delay_slots = BTreeSet::new();
        for at in range.clone().step_by(4) {
            let i = insn(at);
            let flow = i.flow();
            if flow.has_delay_slot() {
                delay_slots.insert(at + 4);
                leaders.insert(at + 8);
            } else if matches!(flow, Flow::Eret) {
                leaders.insert(at + 4);
            }
            match flow {
                Flow::Branch | Flow::BranchLikely => {
                    leaders.insert(i.branch_target(at));
                }
                Flow::Jump => {
                    leaders.insert(i.jump_target(at));
                }
                Flow::JumpReg => {
                    if let Some(t) = table(at) {
                        leaders.extend(t.targets.iter().copied());
                    }
                }
                _ => {}
            }
        }
        // Branch-and-link and calls continue in the same block: the call
        // returns to the instruction after the delay slot, a leader above.
        leaders.retain(|address| range.contains(address));
        let targets_in_delay_slots = leaders.intersection(&delay_slots).copied().collect();

        let starts: Vec<u32> = leaders.into_iter().collect();
        let blocks = starts
            .iter()
            .enumerate()
            .map(|(index, &start)| {
                let end = starts.get(index + 1).copied().unwrap_or(function.end);
                let (edges, exits) = edges(&insn, start, end, &range, table);
                Block {
                    start,
                    end,
                    edges,
                    exits,
                }
            })
            .collect();
        Self {
            blocks,
            targets_in_delay_slots,
        }
    }
}

/// The edges out of the block `[start, end)`.
fn edges<'t>(
    insn: &impl Fn(u32) -> Insn,
    start: u32,
    end: u32,
    function: &std::ops::Range<u32>,
    table: impl Fn(u32) -> Option<&'t JumpTable>,
) -> (Vec<Edge>, bool) {
    let mut edges = Vec::new();
    let fallthrough = |edges: &mut Vec<Edge>| {
        if function.contains(&end) {
            edges.push(Edge {
                to: end,
                kind: EdgeKind::Fallthrough,
            });
        }
    };
    // The control transfer, if any, sits before the delay slot.
    let transfer = |at: u32| match insn(at).flow() {
        Flow::Branch | Flow::BranchLikely | Flow::Jump | Flow::JumpReg => at + 8 == end,
        Flow::Eret => at + 4 == end,
        _ => false,
    };
    let last = [end.wrapping_sub(8), end.wrapping_sub(4)]
        .into_iter()
        .find(|&at| at >= start && transfer(at));
    let Some(at) = last else {
        fallthrough(&mut edges);
        return (edges, false);
    };
    let i = insn(at);
    let inside = |target: u32| function.contains(&target);
    let mut exits = false;
    match i.flow() {
        Flow::Branch | Flow::BranchLikely => {
            let target = i.branch_target(at);
            if inside(target) {
                edges.push(Edge {
                    to: target,
                    kind: EdgeKind::Taken,
                });
            } else {
                exits = true;
            }
            let always = i.op() == Op::Beq && i.rs() == 0 && i.rt() == 0;
            if !always {
                fallthrough(&mut edges);
            }
        }
        Flow::Jump => {
            let target = i.jump_target(at);
            if inside(target) {
                edges.push(Edge {
                    to: target,
                    kind: EdgeKind::Jump,
                });
            } else {
                exits = true;
            }
        }
        Flow::JumpReg => match table(at) {
            Some(t) => {
                let cases: BTreeSet<u32> =
                    t.targets.iter().copied().filter(|&c| inside(c)).collect();
                edges.extend(cases.into_iter().map(|to| Edge {
                    to,
                    kind: EdgeKind::Case,
                }));
            }
            None => exits = true,
        },
        Flow::Eret => exits = true,
        _ => fallthrough(&mut edges),
    }
    (edges, exits)
}
