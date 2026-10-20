// Headless pre-decompile pass: create functions at every Python method entry.
//
// The client's DLLs expose their Python API through PyMethodDef tables
// {char *name; PyCFunction meth; int flags; char *doc}. Those functions are
// referenced only from data, so Ghidra's auto-analysis never creates them and
// they are missing from the decompile. This scans initialised memory for that
// shape (name -> ASCII identifier, meth -> executable, flags in METH_* range)
// and disassembles + creates a function at each meth, named py_<name>.
import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.address.AddressSet;
import ghidra.program.model.listing.Function;
import ghidra.program.model.mem.Memory;
import ghidra.program.model.mem.MemoryBlock;
import ghidra.program.model.symbol.SourceType;

public class PyMethodFunctions extends GhidraScript {
    private boolean isIdentifier(Memory mem, Address a) {
        try {
            int n = 0;
            while (n < 64) {
                byte b = mem.getByte(a.add(n));
                if (b == 0) return n > 0;
                char c = (char) b;
                if (!(Character.isLetterOrDigit(c) || c == '_')) return false;
                n++;
            }
        } catch (Exception e) {
            return false;
        }
        return false;
    }

    private String cstring(Memory mem, Address a) throws Exception {
        StringBuilder sb = new StringBuilder();
        for (int n = 0; n < 64; n++) {
            byte b = mem.getByte(a.add(n));
            if (b == 0) break;
            sb.append((char) b);
        }
        return sb.toString();
    }

    @Override
    public void run() throws Exception {
        Memory mem = currentProgram.getMemory();
        int created = 0, found = 0;
        for (MemoryBlock block : mem.getBlocks()) {
            if (!block.isInitialized() || block.isExecute()) continue;
            Address start = block.getStart();
            long size = block.getSize();
            for (long off = 0; off + 16 <= size; off += 4) {
                Address entry = start.add(off);
                try {
                    Address name = toAddr(mem.getInt(entry) & 0xffffffffL);
                    Address meth = toAddr(mem.getInt(entry.add(4)) & 0xffffffffL);
                    int flags = mem.getInt(entry.add(8));
                    MemoryBlock mb = mem.getBlock(meth);
                    MemoryBlock nb = mem.getBlock(name);
                    if (mb == null || !mb.isExecute() || nb == null || nb.isExecute()) continue;
                    if (flags < 0 || flags > 3) continue;
                    if (!isIdentifier(mem, name)) continue;
                    found++;
                    Function f = getFunctionAt(meth);
                    if (f == null) {
                        new DisassembleCommand(meth, new AddressSet(meth), true).applyTo(currentProgram, monitor);
                        if (new CreateFunctionCmd(meth).applyTo(currentProgram, monitor)) {
                            f = getFunctionAt(meth);
                            created++;
                        }
                    }
                    if (f != null && f.getName().startsWith("FUN_")) {
                        f.setName("py_" + cstring(mem, name), SourceType.ANALYSIS);
                    }
                } catch (Exception e) {
                    // not a table entry
                }
            }
        }
        println("PyMethodDef entries: " + found + ", functions created: " + created);
    }
}
