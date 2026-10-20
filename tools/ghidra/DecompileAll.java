// Headless post-script: write every function's decompiled C to <outdir>/<program>.c
// usage: analyzeHeadless <proj> <name> -import <bin> -scriptPath tools/ghidra -postScript DecompileAll.java <outdir>
import java.io.File;
import java.io.PrintWriter;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.Function;

public class DecompileAll extends GhidraScript {
    @Override
    public void run() throws Exception {
        File out = new File(getScriptArgs()[0], currentProgram.getName() + ".c");
        DecompInterface ifc = new DecompInterface();
        ifc.openProgram(currentProgram);
        int ok = 0, bad = 0;
        try (PrintWriter w = new PrintWriter(out, "UTF-8")) {
            for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
                if (monitor.isCancelled()) break;
                DecompileResults r = ifc.decompileFunction(f, 120, monitor);
                w.println("// ==== " + f.getName() + " @ " + f.getEntryPoint());
                if (r != null && r.decompileCompleted()) {
                    w.println(r.getDecompiledFunction().getC());
                    ok++;
                } else {
                    w.println("// decompile failed: " + (r == null ? "null" : r.getErrorMessage()));
                    bad++;
                }
            }
        }
        println("decompiled " + ok + " functions, " + bad + " failed -> " + out);
    }
}
