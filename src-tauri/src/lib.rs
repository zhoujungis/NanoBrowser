//! Code shared by the browser (`nanobrowser`) and the automation agent
//! (`nanoagent`): the in-process DevTools-protocol bridge, and the hand-rolled
//! websocket framing the docked DevTools panel is served over.

pub mod cdp;
pub mod paths;
pub mod ws;
