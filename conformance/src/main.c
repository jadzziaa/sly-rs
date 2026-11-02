#include "conformance.h"

int main(void)
{
    out_str("conformance 4\n");
    test_alu();
    test_fpu();
    test_mmi();
    test_vu0();
    test_flow();
    out_str("done\n");
    for (;;)
        ;
    return 0;
}
