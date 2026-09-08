FROM sysg-usecase-base

COPY tests/docker/usecase/restart-multi-config-separate/alpha.yaml /usecase/alpha.yaml
COPY tests/docker/usecase/restart-multi-config-separate/beta.yaml /usecase/beta.yaml
COPY tests/docker/usecase/restart-multi-config-separate/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
