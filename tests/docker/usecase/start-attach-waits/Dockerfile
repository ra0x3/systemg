FROM sysg-usecase-base

COPY tests/docker/usecase/start-attach-waits/alpha.yaml /usecase/alpha.yaml
COPY tests/docker/usecase/start-attach-waits/beta.yaml /usecase/beta.yaml
COPY tests/docker/usecase/start-attach-waits/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
