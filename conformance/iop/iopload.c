/* Loads iopconf.irx from host: (the folder this program was started from),
 * as the game loads its modules, then waits while the module runs. */
#include <kernel.h>
#include <loadfile.h>
#include <sifrpc.h>

static void out(const char *s)
{
    while (*s)
        *(volatile unsigned char *)0x1000F180 = *s++;
}

int main(void)
{
    int result = 0;
    SifInitRpc(0);
    out("ee: loading iopconf.irx\n");
    if (SifLoadModule("host:iopconf.irx", 0, 0) < 0)
        out("ee: load failed\n");
    else
        out("ee: loaded\n");
    (void)result;
    for (;;)
        ;
    return 0;
}
