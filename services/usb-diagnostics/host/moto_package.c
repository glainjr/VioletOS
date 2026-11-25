// Developer handset package transfer, signed Radio activation and diagnostics.
#include <libusb.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <time.h>
#include <unistd.h>
#include <string.h>

static uint32_t word(const unsigned char *p) {
    return (uint32_t)p[0] | (uint32_t)p[1]<<8 | (uint32_t)p[2]<<16 | (uint32_t)p[3]<<24;
}
static uint32_t crc(const unsigned char *p,size_t n) {
    uint32_t v=~0u;
    while(n--){ v^=*p++; for(unsigned b=0;b<8;b++)v=(v>>1)^((0u-(v&1u))&0xedb88320u); }
    return ~v;
}
static int transfer(libusb_device_handle *h, uint8_t dir, uint8_t op, uint32_t address,
                    unsigned char *data,uint16_t length) {
    int rc=libusb_control_transfer(h,dir,op,address&65535,address>>16,data,length,3000);
    if(rc==length)return 0;
    fprintf(stderr,"USB request %02x at %u: %s (%d of %u)\n",op,address,
            rc<0?libusb_error_name(rc):"short transfer",rc,length);
    return 1;
}
static int status(libusb_device_handle *h,unsigned char b[24]) {
    if(transfer(h,0xc0,0x62,0,b,24)||memcmp(b,"VPKS",4))return 1;
    printf("Radio CRC %08x, catalog bank %u, inbox ID %u, %u/%u bytes\n",
           word(b+12),word(b+20),word(b+16),word(b+8),word(b+4));
    return 0;
}
static int stage(libusb_device_handle *h,const char *file) {
    FILE *f=fopen(file,"rb");
    if(!f){perror(file);return 1;}
    unsigned char bytes[0x3e000+25];
    size_t n=fread(bytes,1,sizeof bytes,f);
    int bad=ferror(f);fclose(f);
    if(bad || n<24 || n==sizeof bytes || memcmp(bytes,"VPK1",4) || word(bytes+16)!=n-24
       || word(bytes+20)!=crc(bytes+24,n-24)) {
        fprintf(stderr,"Invalid VioletOS package\n");return 1;
    }
    uint32_t sum=crc(bytes,n);
    unsigned char digest[]={sum,sum>>8,sum>>16,sum>>24};
    if(transfer(h,0x40,0x60,(uint32_t)n,digest,4))return 1;
    for(size_t pos=0;pos<n;pos+=40){
        uint16_t count=n-pos<40?(uint16_t)(n-pos):40;
        if(transfer(h,0x40,0x61,(uint32_t)pos,bytes+pos,count))return 1;
    }
    unsigned char reply[24];
    if(status(h,reply)||word(reply+4)!=n||word(reply+8)!=n||word(reply+16)!=word(bytes+8))return 1;
    printf("Staged package CRC %08x.\n",word(bytes+20));
    return 0;
}
static int activate_signed(libusb_device_handle *h, const char *signature) {
    FILE *file = fopen(signature, "rb");
    if (!file) { perror(signature); return 1; }
    unsigned char der[73];
    size_t n = fread(der, 1, sizeof der, file);
    int failed = ferror(file);
    fclose(file);
    if (failed || n <= 40 || n > 72) {
        fputs("Invalid P-256 signature length\n", stderr);
        return 1;
    }
    if (transfer(h,0x40,0x66,0,der,40) ||
        transfer(h,0x40,0x67,0,der+40,(uint16_t)(n-40))) return 1;
    return transfer(h,0x40,0x68,0,NULL,0);
}
int main(int argc,char **argv) {
    int radio = argc == 3 && !strcmp(argv[1], "activate-radio");
    int diagnostics = argc == 3 && !strcmp(argv[1], "activate-diagnostics");
    int confirm = argc == 3 && !strcmp(argv[1], "confirm-diagnostics");
    int activate = radio || diagnostics;
    if((argc!=2 && !activate && !confirm) || (!strcmp(argv[1],"--help"))){fputs("moto-package status | radio-state | stopwatch | provision-wpa2 | join-options | join-wifi | scan-wifi | reboot | reboot-bootloader | bt-state | audio-status | audio=HEX | camera-status | camera=HEX | camera-shot=FILE.pgm | camera-raw=FILE | open-log | open-run=PATH | open-push=NAME | screenshot[=SCALE] | FILE.vpk | activate-radio SIGNATURE | activate-diagnostics SIGNATURE | diagnostics-status | diagnostics-recovery | confirm-diagnostics GENERATION\n",stderr);return 2;}
    libusb_context *ctx;
    if(libusb_init(&ctx)<0)return 1;
    libusb_device **devices;
    ssize_t count=libusb_get_device_list(ctx,&devices);
    int rc=1; libusb_device_handle *chosen=NULL;
    for(ssize_t i=0;i<count;i++) {
        struct libusb_device_descriptor d;
        libusb_device_handle *h=NULL;
        if(libusb_get_device_descriptor(devices[i],&d)||d.idVendor!=0x1209||d.idProduct!=1||
           libusb_open(devices[i],&h)<0)continue;
        unsigned char name[64];
        if(libusb_get_string_descriptor_ascii(h,d.iProduct,name,sizeof name)>0 &&
           !strcmp((char*)name,"VioletOS Diagnostics")){
            if(chosen){libusb_close(h);libusb_close(chosen);chosen=NULL;break;}
            chosen=h;
        }else libusb_close(h);
    }
    if(count>=0)libusb_free_device_list(devices,1);
    if(chosen){
        unsigned char b[24];
        if (activate) {
            if (!status(chosen,b) && word(b+16) == (radio ? 7u : 21u))
                rc = activate_signed(chosen, argv[2]);
        } else if (!strcmp(argv[1], "diagnostics-status")) {
            rc = transfer(chosen, 0xc0, 0x6d, 0, b, 12) || memcmp(b,"VUD1",4);
            if (!rc) printf("Diagnostics CRC %08x, trial %u\n", word(b+4), word(b+8));
        } else if (!strcmp(argv[1], "diagnostics-recovery")) {
            rc = transfer(chosen, 0xc0, 0x6d, 0, b, 16) || memcmp(b,"VUD1",4);
            if (!rc) {
                const char *reasons[] = {"none", "Diagnostics fault", "confirmation timeout", "dependency fault"};
                uint32_t reason = word(b+12);
                printf("Last trial failure: %s\n", reason < 4 ? reasons[reason] : "unknown");
            }
        } else if (confirm) {
            char *end = NULL;
            unsigned long token = strtoul(argv[2], &end, 10);
            if (token && token <= UINT32_MAX && end && !*end)
                rc = transfer(chosen, 0x40, 0x6e, (uint32_t)token, NULL, 0);
        } else if (!strcmp(argv[1],"provision-wpa2")) {
            unsigned char payload[73];
            // nonce (4), SSID length (1), padded SSID (32), PMK (32), UTC seconds (4).
            if (fread(payload,1,sizeof payload,stdin)==sizeof payload &&
                fgetc(stdin)==EOF && payload[4] && payload[4]<=32 && word(payload)) {
                rc=transfer(chosen,0x40,0x6a,word(payload),payload+5,payload[4]) ||
                   transfer(chosen,0x40,0x6b,word(payload),payload+37,36);
            }
            for (size_t i=0;i<sizeof payload;i++) ((volatile unsigned char *)payload)[i]=0;
        } else if (!strcmp(argv[1],"join-options")) {
            // For this boot: a station address (6; zero keeps the random one), then a host name (0-32).
            unsigned char options[39];
            size_t n=fread(options,1,sizeof options,stdin);
            if (n>=6 && n<=38) rc=transfer(chosen,0x40,0x6f,0,options,(uint16_t)n);
        } else if (!strcmp(argv[1],"radio-state")) {
            // Radio's progress words 5-15; word 10 is its stage (70-73: rejoining Wi-Fi).
            unsigned char r[52];
            rc=libusb_control_transfer(chosen,0xc0,0x53,0,0,r,52,3000)!=52 || memcmp(r,"VRD2",4);
            if (!rc) printf("Radio stage %u; words 5-9 %08x %08x %08x, 8-9 %08x %08x, 11-15 %08x %08x %08x %08x %08x\n",
                            word(r+28),word(r+4),word(r+8),word(r+12),word(r+20),word(r+24),word(r+32),word(r+36),word(r+40),word(r+44),word(r+48));
        } else if (!strcmp(argv[1],"stopwatch")) {
            rc=transfer(chosen,0xc0,0x64,0,b,8) || memcmp(b,"VSW1",4);
            if (!rc) printf("Saved stopwatch: %u.%u seconds\n",word(b+4)/10,word(b+4)%10);
        } else if (!strcmp(argv[1],"join-wifi")) {
            uint32_t token=arc4random_uniform(UINT32_MAX)+1;
            token&=0x7fffffffu;
            if (!token) token=1;
            unsigned char result[44];
            if (!transfer(chosen,0x40,0x6c,token,NULL,0)) {
                time_t until=time(NULL)+(getenv("MOTO_WIFI_WAIT") ? atoi(getenv("MOTO_WIFI_WAIT")) : 85);
                while(time(NULL)<until) {
                    if(transfer(chosen,0xc0,0x58,0,result,sizeof result)) break;
                    unsigned state=word(result+4);
                    if(word(result)==token && state) {
                        if(state==1) {printf("Moto HTTPS UUID: %.36s\n",(char *)result+8);rc=0;}
                        else fprintf(stderr,"Moto Wi-Fi step failed (%u)\n",state);
                        break;
                    }
                    usleep(250000);
                }
                if(time(NULL)>=until) fputs("Timed out waiting for the handset Wi-Fi result\n",stderr);
            }
        } else if (!strcmp(argv[1],"bt-state")) {
            // Radio's Bluetooth words (targets/moto-g-pure/bt.rs), then the first bytes the controller sent.
            unsigned char page[64], extra[64];
            rc=libusb_control_transfer(chosen,0xc0,0x74,0,0,page,64,3000)!=64 ||
               libusb_control_transfer(chosen,0xc0,0x74,0,1,extra,64,3000)!=64;
            if (!rc) {
                static const char *states[]={"off","starting","on","failed"};
                unsigned state=word(page+4);
                printf("magic %08x state %s error %08x address %04x%08x version %08x %08x\n",word(page),
                       state<4?states[state]:"?",word(page+8),word(page+52),word(page+48),word(page+56),word(page+60));
                printf("commands %u/%u events %u/%u dropped %u; stp rx %u tx %u bad %u\n",word(page+20),word(page+16),
                       word(page+28),word(page+24),word(page+32),word(page+36),word(page+40),word(page+44));
                unsigned total=word(extra); if (total>1024) total=1024;
                for (uint16_t p=8;p<24 && !rc && (p-8)*64u<total;p++) {
                    rc=libusb_control_transfer(chosen,0xc0,0x74,0,p,page,64,3000)!=64;
                    for (unsigned i=0;i<64 && (p-8)*64u+i<total && !rc;i++) printf("%02x%s",page[i],i%32==31?"\n":" ");
                }
                printf("\n");
            }
        } else if (!strcmp(argv[1],"audio-status")) {
            // Ninety-six status words from the Audio process, printed one per line.
            // Images from before the microphone answer only the first 64.
            unsigned char page[64];
            rc=0;
            for (uint16_t p=0;p<6 && !rc;p++) {
                rc=libusb_control_transfer(chosen,0xc0,0x4f,0,p,page,64,3000)!=64;
                if (rc && p>=4) { rc=0; break; }
                for (unsigned i=0;i<16 && !rc;i++) printf("%2u %08x\n",p*16+i,word(page+4*i));
            }
        } else if (!strncmp(argv[1],"audio=",6)) {
            uint32_t request=(uint32_t)strtoul(argv[1]+6,NULL,16);
            rc=transfer(chosen,0x40,0x4e,request,NULL,0);
            if (!rc) printf("Audio request %08x sent.\n",request);
        } else if (!strcmp(argv[1],"camera-status")) {
            // Ninety-six status words from the Camera process, one per line.
            unsigned char page[64];
            rc=0;
            for (uint16_t p=0;p<6 && !rc;p++) {
                rc=libusb_control_transfer(chosen,0xc0,0x43,0,p,page,64,3000)!=64;
                for (unsigned i=0;i<16 && !rc;i++) printf("%2u %08x\n",p*16+i,word(page+4*i));
            }
        } else if (!strncmp(argv[1],"camera=",7)) {
            uint32_t request=(uint32_t)strtoul(argv[1]+7,NULL,16);
            rc=transfer(chosen,0x40,0x44,request,NULL,0);
            if (!rc) printf("Camera request %08x sent.\n",request);
        } else if (!strncmp(argv[1],"camera-shot=",12)) {
            // Camera's 160 x 90 grey preview of a recent frame, as a PGM.
            static unsigned char preview[160*90];
            unsigned char chunk[64];
            rc=0;
            for (uint16_t c=0;c*64<sizeof(preview) && !rc;c++) {
                rc=libusb_control_transfer(chosen,0xc0,0x42,0,c,chunk,64,3000)!=64;
                if (!rc) memcpy(preview+c*64,chunk,c*64+64<=sizeof(preview)?64:sizeof(preview)-c*64);
            }
            FILE *out=rc?NULL:fopen(argv[1]+12,"wb");
            if (out) {
                fprintf(out,"P5\n160 90\n255\n");
                fwrite(preview,1,sizeof(preview),out);
                fclose(out);
                printf("Wrote %s\n",argv[1]+12);
            } else if (!rc) rc=1;
        } else if (!strncmp(argv[1],"camera-raw=",11)) {
            // Camera's whole 64 KiB preview window, raw.
            static unsigned char window[65536];
            rc=0;
            for (uint16_t c=0;c<1024 && !rc;c++)
                rc=libusb_control_transfer(chosen,0xc0,0x42,0,c,window+c*64,64,3000)!=64;
            FILE *out=rc?NULL:fopen(argv[1]+11,"wb");
            if (out) { fwrite(window,1,sizeof(window),out); fclose(out); printf("Wrote %s\n",argv[1]+11); }
            else if (!rc) rc=1;
        } else if (!strcmp(argv[1],"open-log")) {
            // The Open Runtime's 4 KiB log ring: magic, bytes written, then text.
            unsigned char ring[4096];
            rc=0;
            for (uint16_t c=0;c<64 && !rc;c++)
                rc=libusb_control_transfer(chosen,0xc0,0x4d,0,c,ring+64*c,64,3000)!=64;
            if (!rc && !memcmp(ring,"OLOG",4)) {
                uint32_t head=word(ring+4), size=4088, start=head>size?head%size:0, n=head>size?size:head;
                for (uint32_t i=0;i<n;i++) putchar(ring[8+(start+i)%size]);
            } else if (!rc) { fputs("The Open Runtime has not started.\n",stderr); rc=1; }
            unsigned char fault[56];
            if (libusb_control_transfer(chosen,0xc0,0x4b,0,0,fault,56,3000)==56) {
                if (!memcmp(fault,"FALT",4))
                    printf("Open Runtime fault: label %u, words %08x %08x %08x %08x\n",word(fault+4),word(fault+8),word(fault+12),word(fault+16),word(fault+20));
                fprintf(stderr,"Open Runtime setup: stage %u, slots %#x, loader %s, image %u bytes\n",word(fault+24),word(fault+28),
                        word(fault+32)==0x4e45504f?"OPEN":"refused",word(fault+36));
                if (word(fault+32)!=0x4e45504f)
                    fprintf(stderr,"First reservation in the window: %#010x, %#x bytes; ramdisk %#010x-%#010x\n",
                            word(fault+40),word(fault+44),word(fault+48),word(fault+52));
            }
        } else if (!strncmp(argv[1],"open-push=",10)) {
            // open-push=NAME reads the program from standard input into /apps/NAME.
            const char *name=argv[1]+10;
            size_t nl=strlen(name);
            static unsigned char file[64<<20];
            size_t size=fread(file,1,sizeof file,stdin);
            rc=nl<1||nl>31||!size||size==sizeof file;
            if (rc) fputs("open-push=NAME < FILE (a name of 1-31 characters, under 64 MiB)\n",stderr);
            // Blocks of up to 64 KiB, then an empty block at the end, which
            // swaps the staged copy into place: a failed push changes nothing.
            for (size_t at=0; !rc; at+=65536) {
                size_t block=at>=size?0:size-at<65536?size-at:65536;
                for (size_t pos=0; !rc && pos<block; pos+=4096) {
                    uint16_t count=block-pos<4096?(uint16_t)(block-pos):4096;
                    rc=transfer(chosen,0x40,0x4a,(uint32_t)pos,file+at+pos,count);
                }
                unsigned char commit[40]={0}, state[12];
                uint32_t fields[2]={(uint32_t)(block?at:size),(uint32_t)block};
                memcpy(commit,fields,8);
                memcpy(commit+8,name,nl);
                if (rc || libusb_control_transfer(chosen,0xc0,0x48,0,0,state,12,3000)!=12) { rc=1; break; }
                uint32_t serial=word(state)+1;
                rc=transfer(chosen,0x40,0x49,0,commit,(uint16_t)(8+nl));
                for (int tries=0; !rc; tries++) {
                    if (libusb_control_transfer(chosen,0xc0,0x48,0,0,state,12,3000)==12 && word(state+4)==serial) {
                        rc=word(state+8)!=0;
                        break;
                    }
                    if (tries>2000) { fputs("The Open Runtime did not take the block\n",stderr); rc=1; }
                    usleep(2000);
                }
                if (!block) break;
                fprintf(stderr,"\r%zu of %zu KiB",(at+block)>>10,size>>10);
            }
            if (!rc) printf("\nInstalled /apps/%s (%zu bytes) until the next restart.\n",name,size);
        } else if (!strncmp(argv[1],"open-run=",9)) {
            size_t n=strlen(argv[1]+9);
            rc=n<1||n>40||transfer(chosen,0x40,0x4c,0,(unsigned char *)argv[1]+9,(uint16_t)n);
            if (!rc) printf("Asked the Open Runtime to start %s.\n",argv[1]+9);
        } else if (!strncmp(argv[1],"screenshot",10)) {
            // screenshot[=SCALE] writes the screen, scaled down by SCALE (default 2), as a PPM
            // to standard output, one strip at a time through the install window.
            unsigned scale=argv[1][10]=='='?(unsigned)atoi(argv[1]+11):2;
            if (scale<1||scale>8) scale=2;
            unsigned w=720/scale, h=1600/scale, per=(65536-32)/(w*2);
            static unsigned char image[720*1600*3], strip[65536];
            rc=0;
            for (unsigned k=0; !rc && k*per<h; k++) {
                uint32_t nonce=arc4random_uniform(0x7fffffff)+1;
                char request[40];
                int n=snprintf(request,sizeof request,"@shot %u %u %u",k,scale,nonce);
                rc=transfer(chosen,0x40,0x4c,0,(unsigned char *)request,(uint16_t)n);
                int ready=0;
                for (int tries=0; !rc && tries<2000 && !ready; tries++) {
                    if (libusb_control_transfer(chosen,0xc0,0x47,0,0,strip,64,3000)!=64) { rc=1; break; }
                    ready=!memcmp(strip,"SHOT",4) && word(strip+4)==nonce;
                    if (!ready) usleep(1000);
                }
                if (!rc && !ready) { fputs("The Open Runtime did not answer the screenshot request\n",stderr); rc=1; }
                unsigned rows=ready?word(strip+28):0, bytes=32+rows*w*2;
                for (uint16_t c=1; !rc && c*64u<bytes; c++)
                    rc=libusb_control_transfer(chosen,0xc0,0x47,0,c,strip+64*c,64,3000)!=64;
                for (unsigned i=0; !rc && i<rows*w; i++) {
                    unsigned v=strip[32+2*i]|strip[33+2*i]<<8, at=(k*per*w+i)*3;
                    image[at]=(unsigned char)((v>>11)<<3); image[at+1]=(unsigned char)(((v>>5)&63)<<2); image[at+2]=(unsigned char)((v&31)<<3);
                }
            }
            if (!rc) { printf("P6\n%u %u\n255\n",w,h); fwrite(image,3,(size_t)w*h,stdout); }
        } else if (!strcmp(argv[1],"scan-wifi")) {
            rc=transfer(chosen,0x40,0x69,0,NULL,0);
            if (!rc) puts("Scan requested. Read the result with moto-debug.");
        } else if (!strcmp(argv[1],"reboot-bootloader")) {
            rc=transfer(chosen,0x40,0x70,0,NULL,0);
            if (!rc) puts("Fastboot reboot requested. Wait for the bootloader with fastboot devices.");
        } else if (!strcmp(argv[1],"reboot")) {
            rc=transfer(chosen,0x40,0x65,0,NULL,0);
            if (!rc) puts("Reboot requested. Verify USB reconnect and check the active catalog.");
        } else {
            rc=!strcmp(argv[1],"status")?status(chosen,b):stage(chosen,argv[1]);
        }
        libusb_close(chosen);
    }else fprintf(stderr,"Connect one VioletOS handset with USB diagnostics.\n");
    libusb_exit(ctx);return rc;
}
