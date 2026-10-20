// Headless pre-decompile pass: create functions at code reached only through
// data - C++ vtables, callback tables, Python method tables.
//
// Ghidra's auto-analysis follows calls and jumps; a function whose only
// reference is a pointer in .rdata/.data (every virtual method of the blue
// object reader, for instance) is never created and so never decompiled.
// This walks every aligned dword in initialised non-executable memory and,
// where it points at the start of a clean disassembly in an executable block
// outside any existing function, creates a function there.
import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressSet;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.mem.Memory;
import ghidra.program.model.mem.MemoryBlock;

public class DataRefFunctions extends GhidraScript {
    @Override
    public void run() throws Exception {
        Memory mem = currentProgram.getMemory();
        int created = 0;
        for (MemoryBlock block : mem.getBlocks()) {
            if (!block.isInitialized() || block.isExecute()) continue;
            Address start = block.getStart();
            long size = block.getSize();
            for (long off = 0; off + 4 <= size; off += 4) {
                if (monitor.isCancelled()) return;
                Address target;
                try {
                    target = toAddr(mem.getInt(start.add(off)) & 0xffffffffL);
                } catch (Exception e) {
                    continue;
                }
                MemoryBlock tb = mem.getBlock(target);
                if (tb == null || !tb.isExecute()) continue;
                if (getFunctionContaining(target) != null) continue;
                Instruction existing = getInstructionAt(target);
                if (existing == null) {
                    if (getInstructionContaining(target) != null) continue; // mid-instruction
                    new DisassembleCommand(target, new AddressSet(target), true).applyTo(currentProgram, monitor);
                    if (getInstructionAt(target) == null) continue;
                }
                if (new CreateFunctionCmd(target).applyTo(currentProgram, monitor)) {
                    created++;
                }
            }
        }
        println("functions created from data references: " + created);
    }
}
