// Producer-Consumer Top Module
//
// Wires a producer and consumer through a bounded buffer.
// The producer pushes data when it produces (valid signal),
// and the consumer pops data when it acknowledges (ack wire).
//
// Property of interest: the buffer never overflows (count < 3).
//
// NOTE: `mununu verify` refuses `multi_module = true` until mununu#1249 is
//   fixed (the per-module composition leaves the top's own logic out). Until then set
//   `multi_module = false`: the design is lifted flattened, as one module.
//
// Usage (sv-yosys multi-module path; requires yosys + sv2v):
//   A verify.toml source lists this top module + its submodules and sets
//   [sources.options] multi_module = true (+ optional top). The driver
//   lifts each submodule to a KMTS, renames its ports to the connected
//   nets (from the Yosys netlist), and synchronously composes them; the
//   composed automaton is named `Circuit`.
//
//   [[sources]]
//   id = "system"
//   adapter = "sv-yosys"
//   files = ["multi_producer_consumer_top.sv", "multi_producer.sv",
//            "multi_consumer.sv", "multi_buffer.sv"]
//   [sources.options]
//   multi_module = true
//   top = "producer_consumer_top"
//
// See examples/verify/sv_multi_module/ for a runnable instance of this pattern.

module producer_consumer_top(
    input logic clk,
    input logic rst,
    input logic enable  // environment controls when producer starts
);

    // Internal wires
    logic valid;     // producer -> consumer (data available)
    logic push_sig;  // producer -> buffer (push request)
    logic pop_sig;   // consumer -> buffer (pop request)
    logic full;      // buffer -> (observable output)

    // Producer: IDLE -> PRODUCING -> DONE -> IDLE
    // Outputs valid=1 when PRODUCING
    producer u_producer(
        .clk(clk),
        .rst(rst),
        .enable(enable),
        .valid(valid)
    );

    // Buffer: 2-entry bounded buffer
    // push increments count, pop decrements, full when count >= 2
    bounded_buffer u_buffer(
        .clk(clk),
        .rst(rst),
        .push(valid),
        .pop(pop_sig),
        .full(full)
    );

    // Consumer: IDLE -> BUSY -> ACK -> IDLE
    // Reacts when valid is asserted
    consumer u_consumer(
        .clk(clk),
        .rst(rst),
        .valid(valid)
    );

endmodule
