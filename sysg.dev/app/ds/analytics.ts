export const GTAG_ID = "AW-18405236259";

export const gtagInitScript = `window.dataLayer=window.dataLayer||[];function gtag(){dataLayer.push(arguments)}gtag("js",new Date());gtag("config","${GTAG_ID}")`;

export const CONVERSION_ID = "AW-18405236259/WSWACICjneYcEKO8pshE";

export const conversionScript = `gtag("event","conversion",{send_to:"${CONVERSION_ID}",value:1.0,currency:"USD"})`;
