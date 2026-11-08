// The two capture-timing properties of mununu#545, bound in from outside. The bind statements
// live in line_table_bind.sv and line_table_twin_bind.sv, one per file: a bind to a module that
// is not in the compile is an elaboration error under slang, so each run passes only its own.
//
// Both are about WHEN the captured entry changes: between strobes it must not. Neither names
// a table value. On line_table both hold; on line_table_twin (capture every cycle) both are
// violated whenever `mem[line]`'s low bit differs from the held `enable`, which free `we`,
// `wdata`, `waddr` and `line` can arrange in two cycles.
module line_table_sva (
    input logic clk,
    input logic rst_n,
    input logic line_stb,
    input logic enable
);
    a_enable_holds_between_strobes: assert property (@(posedge clk) disable iff (!rst_n)
        (enable && !line_stb) |=> enable);

    a_disable_holds_between_strobes: assert property (@(posedge clk) disable iff (!rst_n)
        (!enable && !line_stb) |=> !enable);
endmodule

