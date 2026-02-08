import { invoke } from "@tauri-apps/api/core";
import { PowerOffIcon } from "lucide-react";
import { Button } from "@/components/ui/button";

export const KillSwitch = () => {
    const handleQuit = async () => {
        try {
            await invoke("exit_app");
        } catch (error) {
            console.error("Failed to exit app:", error);
        }
    };

    return (
        <div className="space-y-2">
            <div className="text-sm font-medium">Kill Switch</div>
            <div className="flex flex-col gap-2">
                <Button
                    variant="destructive"
                    size="default"
                    onClick={handleQuit}
                    className="w-full font-semibold"
                >
                    <PowerOffIcon className="w-4 h-4 mr-2" />
                    Quit Application
                </Button>
                <p className="text-xs text-muted-foreground">
                    Immediately closes the application
                </p>
            </div>
        </div>
    );
};
